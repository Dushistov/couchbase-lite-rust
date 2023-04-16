use crate::{
    error::{Error, Result},
    ffi::{
        C4IndexType, C4SliceResult, FLDict_Get, FLString, FLTrust, FLValueType, FLValue_AsDict,
        FLValue_AsInt, FLValue_AsString, FLValue_FromData, FLValue_GetType, FLValue_IsInteger,
        _FLDict,
    },
    value::{ValueRef, ValueRefArray},
};
use fallible_streaming_iterator::FallibleStreamingIterator;
use serde_fleece::NonNullConst;

/// Database's index types
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

pub(crate) struct DbIndexesListIterator {
    _enc_data: C4SliceResult,
    array: ValueRefArray,
    next_idx: u32,
    cur_val: Option<IndexInfo>,
}

impl DbIndexesListIterator {
    pub(crate) fn new(enc_data: C4SliceResult) -> Result<Self> {
        let fvalue = unsafe { FLValue_FromData(enc_data.as_fl_slice(), FLTrust::kFLTrusted) };
        let val = unsafe { ValueRef::new(fvalue) };
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

pub struct IndexInfo {
    name: FLString,
    type_: C4IndexType,
    expr: FLString,
}

impl IndexInfo {
    #[inline]
    pub fn name_as_str(&self) -> Result<&str> {
        self.name
            .try_into()
            .map_err(|_: std::str::Utf8Error| Error::InvalidUtf8)
    }
    #[inline]
    pub fn type_(&self) -> C4IndexType {
        self.type_
    }
    #[inline]
    pub fn expr_as_str(&self) -> Result<&str> {
        self.expr
            .try_into()
            .map_err(|_: std::str::Utf8Error| Error::InvalidUtf8)
    }
}

impl TryFrom<NonNullConst<_FLDict>> for IndexInfo {
    type Error = Error;

    fn try_from(dict: NonNullConst<_FLDict>) -> Result<Self> {
        fn get_str(dict: NonNullConst<_FLDict>, key: &str) -> Result<FLString> {
            let s = unsafe { FLDict_Get(dict.as_ptr(), key.into()) };
            let s = NonNullConst::new(s)
                .ok_or_else(|| Error::LogicError(format!("No '{}' key in index info dict", key)))?;
            if unsafe { FLValue_GetType(s.as_ptr()) } != FLValueType::kFLString {
                return Err(Error::LogicError(format!(
                    "Key '{}' in index info dict has not string type",
                    key
                )));
            }
            let s = unsafe { FLValue_AsString(s.as_ptr()) };
            let _s_utf8: &str = s.try_into().map_err(|_| Error::InvalidUtf8)?;
            Ok(s)
        }

        let t = unsafe { FLDict_Get(dict.as_ptr(), "type".into()) };
        let t = NonNullConst::new(t)
            .ok_or_else(|| Error::LogicError("No 'type' key in index info dict".into()))?;
        if !(unsafe { FLValue_GetType(t.as_ptr()) } == FLValueType::kFLNumber
            && unsafe { FLValue_IsInteger(t.as_ptr()) })
        {
            return Err(Error::LogicError(
                "Key 'type' in index info dict has not integer type".into(),
            ));
        }
        let t: u32 = unsafe { FLValue_AsInt(t.as_ptr()) }
            .try_into()
            .map_err(|err| Error::LogicError(format!("Can convert index type to u32: {}", err)))?;

        Ok(Self {
            name: get_str(dict, "name")?,
            type_: C4IndexType(t),
            expr: get_str(dict, "expr")?,
        })
    }
}

impl FallibleStreamingIterator for DbIndexesListIterator {
    type Error = Error;
    type Item = IndexInfo;

    fn advance(&mut self) -> Result<()> {
        if self.next_idx < self.array.len() {
            let val = unsafe { self.array.get_raw(self.next_idx) };
            let val_type = unsafe { FLValue_GetType(val) };
            if val_type != FLValueType::kFLDict {
                return Err(Error::LogicError(format!(
                    "Wrong index type, expect String, got {:?}",
                    val_type
                )));
            }
            let dict = unsafe { FLValue_AsDict(val) };
            let dict = NonNullConst::new(dict).ok_or_else(|| {
                Error::LogicError("Can not convert one index info to Dict".into())
            })?;
            self.cur_val = Some(dict.try_into()?);
            self.next_idx += 1;
        } else {
            self.cur_val = None;
        }
        Ok(())
    }

    #[inline]
    fn get(&self) -> Option<&IndexInfo> {
        self.cur_val.as_ref()
    }
}
