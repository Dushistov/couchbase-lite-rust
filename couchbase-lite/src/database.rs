use crate::{
    doc_enumerator::{DocEnumerator, DocEnumeratorFlags},
    document::{C4DocumentOwner, Document},
    error::{c4error_init, Error, Result},
    ffi::{
        c4db_createIndex, c4db_getDoc, c4db_getDocumentCount, c4db_getIndexesInfo, c4db_getName,
        c4db_getSharedFleeceEncoder, c4db_openNamed, c4db_release, kC4DB_Create, kC4DB_NoUpgrade,
        kC4DB_NonObservable, kC4DB_ReadOnly, C4Database, C4DatabaseConfig2, C4DocContentLevel,
        C4DocumentEnded, C4EncryptionAlgorithm, C4EncryptionKey, C4ErrorCode, C4ErrorDomain,
        C4IndexOptions, C4IndexType, C4RevisionFlags, FLDict,
    },
    index::{DbIndexesListIterator, IndexInfo, IndexOptions, IndexType},
    log_reroute::c4log_to_log_init,
    observer::{DatabaseObserver, ObserverdChangesIter},
    query::Query,
    replicator::{Replicator, ReplicatorAuthentication, ReplicatorState},
    transaction::Transaction,
    QueryLanguage,
};
use bitflags::bitflags;
use fallible_streaming_iterator::FallibleStreamingIterator;
use log::{debug, error, trace};
use serde_fleece::FlEncoderSession;
use std::{
    collections::HashSet,
    ffi::CString,
    marker::PhantomData,
    path::Path,
    ptr::{self, NonNull},
    sync::{Arc, Mutex, Once},
};

/// Database configuration, used during open
pub struct DatabaseConfig<'a> {
    inner: Result<C4DatabaseConfig2>,
    phantom: PhantomData<&'a Path>,
}

bitflags! {
    #[repr(transparent)]
    pub struct DatabaseFlags: u32 {
        /// Create the file if it doesn't exist
        const CREATE = kC4DB_Create;
        /// Open file read-only
        const READ_ONLY = kC4DB_ReadOnly;
        /// Disable upgrading an older-version database
        const NO_UPGRADE = kC4DB_NoUpgrade;
        /// Disable database/collection observers, for slightly faster writes
        const NON_OBSERVABLE = kC4DB_NonObservable;
    }
}

impl<'a> DatabaseConfig<'a> {
    pub fn new(parent_directory: &'a Path, flags: DatabaseFlags) -> Self {
        let os_path_utf8 = match parent_directory.to_str() {
            Some(x) => x,
            None => {
                return Self {
                    inner: Err(Error::InvalidUtf8),
                    phantom: PhantomData,
                }
            }
        };
        Self {
            inner: Ok(C4DatabaseConfig2 {
                parentDirectory: os_path_utf8.into(),
                flags: flags.bits(),
                encryptionKey: C4EncryptionKey {
                    algorithm: C4EncryptionAlgorithm::kC4EncryptionNone,
                    bytes: [0; 32],
                },
            }),
            phantom: PhantomData,
        }
    }
}

/// A connection to a couchbase-lite database.
pub struct Database {
    pub(crate) inner: DbInner,
    pub(crate) db_events: Arc<Mutex<HashSet<usize>>>,
    pub(crate) db_observers: Vec<DatabaseObserver>,
    db_replicator: Option<Replicator>,
    replicator_params: Option<ReplicatorParams>,
}

struct ReplicatorParams {
    url: String,
    auth: ReplicatorAuthentication,
}

pub(crate) struct DbInner(pub NonNull<C4Database>);
/// According to
/// https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
/// it is possible to call from any thread, but not concurrently
unsafe impl Send for DbInner {}

impl Drop for DbInner {
    fn drop(&mut self) {
        trace!("release db {:?}", self.0.as_ptr());
        unsafe { c4db_release(self.0.as_ptr()) };
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if let Some(repl) = self.db_replicator.take() {
            repl.stop();
        }
        self.db_observers.clear();
    }
}

impl Database {
    pub fn open_named(name: &str, cfg: DatabaseConfig) -> Result<Self> {
        DB_LOG_HANDLER.call_once(|| {
            debug!("init couchbase log to rust log rerouting");
            c4log_to_log_init();
        });
        let cfg = cfg.inner?;
        let mut error = c4error_init();
        let db_ptr = unsafe { c4db_openNamed(name.into(), &cfg, &mut error) };
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
    pub fn open_with_flags(path: &Path, flags: DatabaseFlags) -> Result<Self> {
        let parent_path = path
            .parent()
            .ok_or_else(|| Error::LogicError(format!("path {:?} has no parent diretory", path)))?;
        let cfg = DatabaseConfig::new(parent_path, flags);
        let db_name = path
            .file_name()
            .ok_or_else(|| Error::LogicError(format!("path {:?} has no last part", path)))?
            .to_str()
            .ok_or(Error::InvalidUtf8)?
            .strip_suffix(".cblite2")
            .ok_or_else(|| {
                Error::LogicError(format!(
                    "path {:?} should have last part with .cblite2 suffix",
                    path
                ))
            })?;

        Database::open_named(db_name, cfg)
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
        Query::new(self, QueryLanguage::kC4JSONQuery, query_json)
    }
    /// Compiles a query from an expression given as N1QL.
    pub fn n1ql_query(&self, query: &str) -> Result<Query> {
        Query::new(self, QueryLanguage::kC4N1QLQuery, query)
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

    /// Intialize socket implementation for replication
    /// (builtin couchbase-lite websocket library)
    #[cfg(feature = "use-couchbase-lite-websocket")]
    pub fn init_socket_impl() {
        crate::replicator::init_builtin_socket_impl();
    }

    /// Intialize socket implementation for replication
    /// (builtin couchbase-lite websocket library)
    #[cfg(feature = "use-tokio-websocket")]
    pub fn init_socket_impl(handle: tokio::runtime::Handle) {
        crate::replicator::init_tokio_socket_impl(handle);
    }

    /// starts database replication
    /// * `reset` - If true, the replicator will reset its checkpoint
    ///             and start replication from the beginning.
    /// * `validation_cb` - Callback that can reject incoming revisions.
    ///    Arguments: collection_name, doc_id, rev_id, rev_flags, doc_body.
    ///    It should return false to reject document.
    /// * `repl_status_changed` - Callback to be invoked when replicator's status changes.
    /// * `repl_docs_ended` - Callback notifying status of individual documents.
    pub fn start_replicator<StatusF, DocsReplF, ValidationF>(
        &mut self,
        url: &str,
        auth: ReplicatorAuthentication,
        reset: bool,
        validation_cb: ValidationF,
        mut repl_status_changed: StatusF,
        repl_docs_ended: DocsReplF,
    ) -> Result<()>
    where
        ValidationF: FnMut(&str, &str, &str, C4RevisionFlags, FLDict) -> bool + Send + 'static,
        StatusF: FnMut(ReplicatorState) + Send + 'static,
        DocsReplF: FnMut(bool, &mut dyn Iterator<Item = &C4DocumentEnded>) + Send + 'static,
    {
        let mut db_replicator = Replicator::new(
            self,
            url,
            auth.clone(),
            validation_cb,
            move |status| match ReplicatorState::try_from(status) {
                Ok(state) => repl_status_changed(state),
                Err(err) => {
                    error!("replicator status change: invalid status {}", err);
                }
            },
            repl_docs_ended,
        )?;
        db_replicator.start(reset)?;
        self.db_replicator = Some(db_replicator);
        self.replicator_params = Some(ReplicatorParams {
            url: url.into(),
            auth,
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
        self.db_replicator =
            Some(repl.restart(self, &replicator_params.url, replicator_params.auth.clone())?);
        Ok(())
    }

    /// stop database replication
    pub fn stop_replicator(&mut self) {
        if let Some(repl) = self.db_replicator.take() {
            repl.stop();
        }
    }

    /// Get shared "fleece" encoder, `&mut self` to make possible
    /// exists only one session
    pub fn shared_encoder_session(&mut self) -> Result<FlEncoderSession> {
        let enc = unsafe { c4db_getSharedFleeceEncoder(self.inner.0.as_ptr()) };
        NonNull::new(enc)
            .ok_or_else(|| {
                Error::LogicError("c4db_getSharedFleeceEncoder return null.into()".into())
            })
            .map(FlEncoderSession::new)
    }

    /// Returns the names of all indexes in the database
    pub fn get_indexes(
        &self,
    ) -> Result<impl FallibleStreamingIterator<Item = IndexInfo, Error = Error>> {
        let mut c4err = c4error_init();
        let enc_data = unsafe { c4db_getIndexesInfo(self.inner.0.as_ptr(), &mut c4err) };
        if enc_data.buf.is_null() {
            return Err(c4err.into());
        }

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
            ValueIndex => C4IndexType::kC4ValueIndex,
            FullTextIndex => C4IndexType::kC4FullTextIndex,
            ArrayIndex => C4IndexType::kC4ArrayIndex,
            PredictiveIndex => C4IndexType::kC4PredictiveIndex,
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
                    list.push_str(word);
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
                    index_name.into(),
                    expression_json.into(),
                    index_type,
                    &opts,
                    &mut c4err,
                )
            }
        } else {
            unsafe {
                c4db_createIndex(
                    self.inner.0.as_ptr(),
                    index_name.into(),
                    expression_json.into(),
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

    /// Returns the name of the database, as given to `c4db_openNamed`.
    /// This is the filename _without_ the ".cblite2" extension.
    #[inline]
    pub fn name(&self) -> Result<&str> {
        unsafe { c4db_getName(self.inner.0.as_ptr()) }
            .try_into()
            .map_err(|_| Error::InvalidUtf8)
    }

    pub(crate) fn do_internal_get(
        &self,
        doc_id: &str,
        must_exists: bool,
        content_level: C4DocContentLevel,
    ) -> Result<C4DocumentOwner> {
        let mut c4err = c4error_init();
        let c4doc = unsafe {
            c4db_getDoc(
                self.inner.0.as_ptr(),
                doc_id.as_bytes().into(),
                must_exists,
                content_level,
                &mut c4err,
            )
        };
        NonNull::new(c4doc)
            .ok_or_else(|| c4err.into())
            .map(C4DocumentOwner)
    }

    pub(crate) fn do_internal_get_opt(
        &self,
        doc_id: &str,
        must_exists: bool,
        content_level: C4DocContentLevel,
    ) -> Result<Option<C4DocumentOwner>> {
        match self.do_internal_get(doc_id, must_exists, content_level) {
            Ok(x) => Ok(Some(x)),
            Err(Error::C4Error(err))
                if err.domain == C4ErrorDomain::LiteCoreDomain
                    && err.code == C4ErrorCode::kC4ErrorNotFound.0 =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    #[inline]
    pub(crate) fn internal_get(&self, doc_id: &str, must_exists: bool) -> Result<C4DocumentOwner> {
        self.do_internal_get(doc_id, must_exists, C4DocContentLevel::kDocGetCurrentRev)
    }
}

static DB_LOG_HANDLER: Once = Once::new();
