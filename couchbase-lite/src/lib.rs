//! couchbase-lite is an ergonomic wrapper for using couchbase-lite-core from Rust.
//! ```rust
//! # #[macro_use]
//! # extern crate serde;
//! # use serde::{Serialize, Deserialize};
//! use couchbase_lite::{
//!     Database, DatabaseConfig, Document,
//!     fallible_streaming_iterator::FallibleStreamingIterator
//! };
//! use std::path::Path;
//!
//! #[derive(Serialize, Deserialize, Debug)]
//! #[serde(tag = "type")]
//! struct Message {
//!     msg: String,
//! }
//!
//! fn main() -> Result<(), couchbase_lite::Error> {
//!     let mut db = Database::open(Path::new("a.cblite2"), DatabaseConfig::default())?;
//!     {
//!         let msg = Message { msg: "Test message".into() };
//!         let mut trans = db.transaction()?;
//!         let mut doc = Document::new(&msg)?;
//!         trans.save(&mut doc)?;
//!         trans.commit()?;
//!     }
//!     println!("we have {} documents in db", db.document_count());
//!     let query = db.query(r#"{"WHAT": ["._id"], "WHERE": ["=", [".type"], "Message"]}"#)?;
//!     let mut iter = query.run()?;
//!     while let Some(item) = iter.next()? {
//!         let id = item.get_raw_checked(0)?;
//!         let id = id.as_str()?;
//!         let doc = db.get_existing(id)?;
//!         println!("doc id {}", doc.id());
//!         let db_msg: Message = doc.decode_data()?;
//!         println!("db_msg: {:?}", db_msg);
//!         assert_eq!("Test message", db_msg.msg);
//!     }
//!     Ok(())
//! }
//! ```

mod doc_enumerator;
mod document;
mod error;
mod fl_slice;
mod log_reroute;
mod observer;
mod query;
mod replicator;
mod transaction;
mod value;

pub use crate::{
    doc_enumerator::{DocEnumerator, DocEnumeratorFlags},
    document::Document,
    error::Error,
    query::Query,
    replicator::ReplicatorState,
};
pub use couchbase_lite_core_sys as ffi;
pub use fallible_streaming_iterator;

use crate::{
    document::C4DocumentOwner,
    error::{c4error_init, Result},
    ffi::{
        c4db_createIndex, c4db_free, c4db_getDocumentCount, c4db_getIndexes, c4db_open, c4doc_get,
        c4socket_registerFactory, kC4ArrayIndex, kC4DB_Create, kC4EncryptionNone, kC4FullTextIndex,
        kC4PredictiveIndex, kC4RevisionTrees, kC4SQLiteStorageEngine, kC4ValueIndex,
        C4CivetWebSocketFactory, C4Database, C4DatabaseConfig, C4DatabaseFlags,
        C4DocumentVersioning, C4EncryptionAlgorithm, C4EncryptionKey, C4IndexOptions, C4String,
        FLTrust_kFLTrusted, FLValue, FLValueType, FLValue_AsString, FLValue_FromData,
        FLValue_GetType,
    },
    fl_slice::{fl_slice_to_str_unchecked, AsFlSlice, FlSliceOwner},
    log_reroute::DB_LOGGER,
    observer::{DatabaseObserver, DbChange, DbChangesIter},
    replicator::Replicator,
    transaction::Transaction,
    value::{ValueRef, ValueRefArray},
};
use fallible_streaming_iterator::FallibleStreamingIterator;
use log::error;
use once_cell::sync::Lazy;
use std::{
    collections::HashSet,
    convert::{TryFrom, TryInto},
    ffi::CString,
    path::Path,
    ptr,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

/// Database configuration, used during open
pub struct DatabaseConfig {
    inner: C4DatabaseConfig,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            inner: C4DatabaseConfig {
                flags: kC4DB_Create as C4DatabaseFlags,
                storageEngine: unsafe { kC4SQLiteStorageEngine },
                versioning: kC4RevisionTrees as C4DocumentVersioning,
                encryptionKey: C4EncryptionKey {
                    algorithm: kC4EncryptionNone as C4EncryptionAlgorithm,
                    bytes: [0; 32],
                },
            },
        }
    }
}

/// use embedded web-socket library
pub fn use_c4_civet_web_socket_factory() {
    unsafe { c4socket_registerFactory(C4CivetWebSocketFactory) };
}

/// A connection to a couchbase-lite database.
pub struct Database {
    inner: DbInner,
    db_events: Arc<Mutex<HashSet<usize>>>,
    db_observers: Vec<DatabaseObserver>,
    db_replicator: Option<Replicator>,
    replicator_params: Option<ReplicatorParams>,
}

struct ReplicatorParams {
    url: String,
    token: Option<String>,
}

pub(crate) struct DbInner(NonNull<C4Database>);
/// According to
/// https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
/// it is possible to call from any thread, but not concurrently
unsafe impl Send for DbInner {}

impl Drop for Database {
    fn drop(&mut self) {
        if let Some(repl) = self.db_replicator.take() {
            repl.stop();
        }
        self.db_observers.clear();
        unsafe { c4db_free(self.inner.0.as_ptr()) };
    }
}

impl Database {
    pub fn open(path: &Path, cfg: DatabaseConfig) -> Result<Database> {
        Lazy::force(&DB_LOGGER);
        let mut error = c4error_init();
        let os_path_utf8 = path.to_str().ok_or(Error::Utf8)?;
        let os_path_utf8: C4String = os_path_utf8.as_flslice();
        let db_ptr = unsafe { c4db_open(os_path_utf8, &cfg.inner, &mut error) };
        NonNull::new(db_ptr)
            .map(|inner| Database {
                inner: DbInner(inner),
                db_events: Arc::new(Mutex::new(HashSet::new())),
                db_observers: vec![],
                db_replicator: None,
                replicator_params: None,
            })
            .ok_or_else(|| error.into())
    }

    pub(crate) fn internal_get(&self, doc_id: &str, must_exists: bool) -> Result<C4DocumentOwner> {
        let mut c4err = c4error_init();
        let doc_ptr = unsafe {
            c4doc_get(
                self.inner.0.as_ptr(),
                doc_id.as_bytes().as_flslice(),
                must_exists,
                &mut c4err,
            )
        };
        NonNull::new(doc_ptr)
            .map(C4DocumentOwner)
            .ok_or_else(|| c4err.into())
    }

    /// Begin a new transaction, the transaction defaults to rolling back
    /// when it is dropped. If you want the transaction to commit,
    /// you must call `Transaction::commit`
    pub fn transaction(&mut self) -> Result<Transaction> {
        Transaction::new(self)
    }
    /// Returns the number of (undeleted) documents in the database
    pub fn document_count(&self) -> u64 {
        unsafe { c4db_getDocumentCount(self.inner.0.as_ptr()) }
    }

    /// Return existing document from database
    pub fn get_existing(&self, doc_id: &str) -> Result<Document> {
        self.internal_get(doc_id, true)
            .map(|x| Document::new_internal(x, doc_id))
    }
    /// Compiles a query from an expression given as JSON.
    /// The expression is a predicate that describes which documents should be returned.
    /// A separate, optional sort expression describes the ordering of the results.
    pub fn query(&self, query_json: &str) -> Result<Query> {
        Query::new(self, query_json)
    }
    /// Compiles a query from an expression given as N1QL.
    pub fn n1ql_query(&self, query: &str) -> Result<Query> {
        Query::new_n1ql(self, query)
    }

    /// Creates an enumerator ordered by docID.
    pub fn enumerate_all_docs(&self, flags: DocEnumeratorFlags) -> Result<DocEnumerator> {
        DocEnumerator::enumerate_all_docs(self, flags)
    }

    /// Register a database observer, with a callback that will be invoked after the database
    /// changes. The callback will be called _once_, after the first change. After that it won't
    /// be called again until all of the changes have been read by calling `Database::observed_changes`.
    pub fn register_observer<F>(&mut self, mut callback_f: F) -> Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        let db_events = self.db_events.clone();
        let obs = DatabaseObserver::new(self, move |obs| {
            {
                match db_events.lock() {
                    Ok(mut db_events) => {
                        db_events.insert(obs as usize);
                    }
                    Err(err) => {
                        error!(
                            "register_observer::DatabaseObserver::lambda db_events lock failed: {}",
                            err
                        );
                    }
                }
            }
            callback_f();
        })?;
        self.db_observers.push(obs);
        Ok(())
    }

    /// Remove all database observers
    pub fn clear_observers(&mut self) {
        self.db_observers.clear();
    }

    /// Get observed changes for this database
    pub fn observed_changes(&mut self) -> ObserverdChangesIter {
        ObserverdChangesIter {
            db: self,
            obs_it: None,
        }
    }

    pub fn replicator_state(&self) -> Result<ReplicatorState> {
        match self.db_replicator.as_ref() {
            Some(repl) => repl.status().try_into(),
            None => Ok(ReplicatorState::Offline),
        }
    }

    /// starts database replication
    pub fn start_replicator<F>(
        &mut self,
        url: &str,
        token: Option<&str>,
        mut repl_status_changed: F,
    ) -> Result<()>
    where
        F: FnMut(ReplicatorState) + Send + 'static,
    {
        self.db_replicator =
            Some(Replicator::new(
                self,
                url,
                token,
                move |status| match ReplicatorState::try_from(status) {
                    Ok(state) => repl_status_changed(state),
                    Err(err) => {
                        error!("replicator status change: invalid status {}", err);
                    }
                },
            )?);
        self.replicator_params = Some(ReplicatorParams {
            url: url.into(),
            token: token.map(str::to_string),
        });
        Ok(())
    }
    /// restart database replicator, gives error if `Database::start_replicator`
    /// haven't called yet
    pub fn restart_replicator(&mut self) -> Result<()> {
        let replicator_params = self.replicator_params.as_ref().ok_or_else(|| {
            Error::LogicError(
                "you call restart_replicator, but have not yet call start_replicator (params)"
                    .into(),
            )
        })?;
        let repl = self.db_replicator.take().ok_or_else(|| {
            Error::LogicError(
                "you call restart_replicator, but have not yet call start_replicator (repl)".into(),
            )
        })?;
        self.db_replicator = Some(repl.restart(
            self,
            &replicator_params.url,
            replicator_params.token.as_ref().map(String::as_str),
        )?);
        Ok(())
    }

    /// stop database replication
    pub fn stop_replicator(&mut self) {
        if let Some(repl) = self.db_replicator.take() {
            repl.stop();
        }
    }

    /// Returns the names of all indexes in the database
    pub fn get_indexes(&self) -> Result<impl FallibleStreamingIterator<Item = str, Error = Error>> {
        let mut c4err = c4error_init();
        let enc_data = unsafe { c4db_getIndexes(self.inner.0.as_ptr(), &mut c4err) };
        if enc_data.buf.is_null() {
            return Err(c4err.into());
        }

        let enc_data: FlSliceOwner = enc_data.into();
        let indexes_list = DbIndexesListIterator::new(enc_data)?;
        Ok(indexes_list)
    }

    /// Creates a database index, of the values of specific expressions across
    /// all documents. The name is used to identify the index for later updating
    /// or deletion; if an index with the same name already exists, it will be
    /// replaced unless it has the exact same expressions.
    /// Note: If some documents are missing the values to be indexed,
    /// those documents will just be omitted from the index. It's not an error.
    pub fn create_index(
        &mut self,
        index_name: &str,
        expression_json: &str,
        index_type: IndexType,
        index_options: Option<IndexOptions>,
    ) -> Result<()> {
        use IndexType::*;
        let index_type = match index_type {
            ValueIndex => kC4ValueIndex,
            FullTextIndex => kC4FullTextIndex,
            ArrayIndex => kC4ArrayIndex,
            PredictiveIndex => kC4PredictiveIndex,
        };
        let mut c4err = c4error_init();
        let result = if let Some(index_options) = index_options {
            let language = CString::new(index_options.language)?;
            let stop_words: Option<CString> = if let Some(stop_words) = index_options.stop_words {
                let mut list = String::with_capacity(stop_words.len() * 5);
                for word in stop_words {
                    if !list.is_empty() {
                        list.push(' ');
                    }
                    list.push_str(&word);
                }
                Some(CString::new(list)?)
            } else {
                None
            };

            let opts = C4IndexOptions {
                language: language.as_ptr(),
                disableStemming: index_options.disable_stemming,
                ignoreDiacritics: index_options.ignore_diacritics,
                stopWords: stop_words.map_or(ptr::null(), |x| x.as_ptr()),
            };
            unsafe {
                c4db_createIndex(
                    self.inner.0.as_ptr(),
                    index_name.as_flslice(),
                    expression_json.as_flslice(),
                    index_type,
                    &opts,
                    &mut c4err,
                )
            }
        } else {
            unsafe {
                c4db_createIndex(
                    self.inner.0.as_ptr(),
                    index_name.as_flslice(),
                    expression_json.as_flslice(),
                    index_type,
                    ptr::null(),
                    &mut c4err,
                )
            }
        };
        if result {
            Ok(())
        } else {
            Err(c4err.into())
        }
    }
}

pub struct ObserverdChangesIter<'db> {
    db: &'db Database,
    obs_it: Option<DbChangesIter<'db>>,
}

impl<'db> Iterator for ObserverdChangesIter<'db> {
    type Item = DbChange;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(obs_it) = self.obs_it.as_mut() {
                if let Some(item) = obs_it.next() {
                    return Some(item);
                }
                self.obs_it = None;
            }
            let obs_ptr = {
                let mut db_events = self.db.db_events.lock().expect("db_events lock failed");
                if db_events.is_empty() {
                    return None;
                }
                let obs_ptr = match db_events.iter().next() {
                    Some(obs_ptr) => *obs_ptr,
                    None => return None,
                };
                db_events.remove(&obs_ptr);
                obs_ptr
            };
            let obs: Option<&'db DatabaseObserver> = self
                .db
                .db_observers
                .iter()
                .find(|obs| obs.match_obs_ptr(obs_ptr));
            if let Some(obs) = obs {
                self.obs_it = Some(obs.changes_iter());
            }
        }
    }
}

struct DbIndexesListIterator {
    _enc_data: FlSliceOwner,
    array: ValueRefArray,
    next_idx: u32,
    cur_val: Option<FLValue>,
}

impl DbIndexesListIterator {
    fn new(enc_data: FlSliceOwner) -> Result<Self> {
        let fvalue = unsafe { FLValue_FromData(enc_data.as_flslice(), FLTrust_kFLTrusted) };
        let val: ValueRef = fvalue.into();
        let array = match val {
            ValueRef::Array(arr) => arr,
            _ => {
                return Err(Error::LogicError(
                    "db indexes are not fleece encoded array".into(),
                ))
            }
        };

        Ok(Self {
            _enc_data: enc_data,
            array,
            next_idx: 0,
            cur_val: None,
        })
    }
}

impl FallibleStreamingIterator for DbIndexesListIterator {
    type Error = Error;
    type Item = str;

    fn advance(&mut self) -> Result<()> {
        if self.next_idx < self.array.len() {
            let val = unsafe { self.array.get_raw(self.next_idx) };
            let val_type = unsafe { FLValue_GetType(val) };
            if val_type != FLValueType::kFLString {
                return Err(Error::LogicError(format!(
                    "Wrong index type, expect String, got {:?}",
                    val_type
                )));
            }
            self.cur_val = Some(val);
            self.next_idx += 1;
        } else {
            self.cur_val = None;
        }
        Ok(())
    }

    fn get(&self) -> Option<&str> {
        if let Some(val) = self.cur_val {
            Some(unsafe { fl_slice_to_str_unchecked(FLValue_AsString(val)) })
        } else {
            None
        }
    }
}

pub enum IndexType {
    /// Regular index of property value
    ValueIndex,
    /// Full-text index
    FullTextIndex,
    /// Index of array values, for use with UNNEST
    ArrayIndex,
    /// Index of prediction() results (Enterprise Edition only)
    PredictiveIndex,
}

#[derive(Default)]
pub struct IndexOptions<'a> {
    /// Dominant language of text to be indexed; setting this enables word stemming, i.e.
    /// matching different cases of the same word ("big" and "bigger", for instance.)
    /// Can be an ISO-639 language code or a lowercase (English) language name; supported
    /// languages are: da/danish, nl/dutch, en/english, fi/finnish, fr/french, de/german,
    /// hu/hungarian, it/italian, no/norwegian, pt/portuguese, ro/romanian, ru/russian,
    /// es/spanish, sv/swedish, tr/turkish.
    /// If left empty,  or set to an unrecognized language, no language-specific behaviors
    /// such as stemming and stop-word removal occur.
    pub language: &'a str,
    /// Should diacritical marks (accents) be ignored? Defaults to false.
    /// Generally this should be left false for non-English text.
    pub ignore_diacritics: bool,
    /// "Stemming" coalesces different grammatical forms of the same word ("big" and "bigger",
    /// for instance.) Full-text search normally uses stemming if the language is one for
    /// which stemming rules are available, but this flag can be set to `true` to disable it.
    /// Stemming is currently available for these languages: da/danish, nl/dutch, en/english,
    /// fi/finnish, fr/french, de/german, hu/hungarian, it/italian, no/norwegian, pt/portuguese,
    /// ro/romanian, ru/russian, s/spanish, sv/swedish, tr/turkish.
    pub disable_stemming: bool,
    /// List of words to ignore ("stop words") for full-text search. Ignoring common words
    /// like "the" and "a" helps keep down the size of the index.
    /// If `None`, a default word list will be used based on the `language` option, if there is
    /// one for that language.
    /// To suppress stop-words, use an empty list.
    /// To provide a custom list of words, use the words in lowercase
    /// separated by spaces.
    pub stop_words: Option<&'a [&'a str]>,
}
