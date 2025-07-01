use crate::ffi::{FLEncoder_WriteBool, FLEncoder_WriteInt, FLEncoder_WriteString, _FLEncoder};
use std::ptr::NonNull;

mod private {
    pub trait Sealed {}
}

// Not public API.
#[doc(hidden)]
pub trait EncodeValue: private::Sealed {
    fn encode(&self, enc: NonNull<_FLEncoder>) -> bool;
}

impl private::Sealed for &str {}
impl EncodeValue for &str {
    #[inline]
    fn encode(&self, enc: NonNull<_FLEncoder>) -> bool {
        unsafe { FLEncoder_WriteString(enc.as_ptr(), (*self).into()) }
    }
}

impl private::Sealed for i64 {}
impl EncodeValue for i64 {
    #[inline]
    fn encode(&self, enc: NonNull<_FLEncoder>) -> bool {
        unsafe { FLEncoder_WriteInt(enc.as_ptr(), *self) }
    }
}

impl private::Sealed for String {}
impl EncodeValue for String {
    #[inline]
    fn encode(&self, enc: NonNull<_FLEncoder>) -> bool {
        self.as_str().encode(enc)
    }
}

impl private::Sealed for bool {}
impl EncodeValue for bool {
    #[inline]
    fn encode(&self, enc: NonNull<_FLEncoder>) -> bool {
        unsafe { FLEncoder_WriteBool(enc.as_ptr(), *self) }
    }
}

/// Macros to simplify creation of fleece encoded data
#[macro_export]
macro_rules! fleece {
    //////////////////////////////////////////////////////////////////////////
    // TT muncher for parsing the inside of an array [...].
    //
    // Must be invoked as: fleece!(@array [$($tt)*])
    //////////////////////////////////////////////////////////////////////////
    // Done with trailing comma.
    (@array $enc:ident $all_ok:ident [$($elems:expr,)*]) => {
        $($all_ok &= $crate::EncodeValue::encode(& $elems, $enc);)*
    };

    // Done without trailing comma.
    (@array $enc:ident $all_ok:ident [$($elems:expr),*]) => {
        $($all_ok &= $crate::EncodeValue::encode(& $elems, $enc);)*
    };


    //////////////////////////////////////////////////////////////////////////
    // TT muncher for parsing the inside of an object {...}. Each entry is
    // inserted into the given map variable.
    //
    // Must be invoked as: fleece!(@object $map () ($($tt)*) ($($tt)*))
    //
    // We require two copies of the input tokens so that we can match on one
    // copy and trigger errors on the other copy.
    //////////////////////////////////////////////////////////////////////////

    // Done.
    (@object $enc:ident $all_ok:ident () () ()) => {};

    // Insert the current entry followed by trailing comma.
    (@object $enc:ident $all_ok:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        $all_ok &= $crate::ffi::FLEncoder_WriteKey($enc.as_ptr(), ($($key)+).into());
        $all_ok &= $crate::EncodeValue::encode(& $value, $enc);
        $crate::fleece!(@object $enc $all_ok () ($($rest)*) ($($rest)*));
    };

    // Current entry followed by unexpected token.
    (@object $enc:ident $all_ok:ident [$($key:tt)+] ($value:expr) $unexpected:tt $($rest:tt)*) => {
        fleece_unexpected!($unexpected);
    };

    // Insert the last entry without trailing comma.
    (@object $enc:ident $all_ok:ident [$($key:tt)+] ($value:expr)) => {
        $all_ok &= $crate::ffi::FLEncoder_WriteKey($enc.as_ptr(), ($($key)+).into());
        $all_ok &= $crate::EncodeValue::encode(& $value, $enc);
    };

    // Next value is `null`.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::fleece!(@object $enc $all_ok [$($key)+] (fleece!(null)) $($rest)*);
    };
    // Next value is an array without trailing comma
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: [$($array:tt)*]) $copy:tt) => {
        $all_ok &= $crate::ffi::FLEncoder_WriteKey($enc.as_ptr(), ($($key)+).into());
        $all_ok &= $crate::ffi::FLEncoder_BeginArray($enc.as_ptr(), count_tts!($($array)*));
        $crate::fleece!(@array $enc $all_ok [$($array)*]);
        $all_ok &= $crate::ffi::FLEncoder_EndArray($enc.as_ptr());
    };
    // Next value is an array with trailing comma
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: [$($array:tt)*] , $($rest:tt)+) $copy:tt) => {
        $all_ok &= $crate::ffi::FLEncoder_WriteKey($enc.as_ptr(), ($($key)+).into());
        $all_ok &= $crate::ffi::FLEncoder_BeginArray($enc.as_ptr(), count_tts!($($array)*));
        $crate::fleece!(@array $enc $all_ok [$($array)*]);
        $all_ok &= $crate::ffi::FLEncoder_EndArray($enc.as_ptr());
        $crate::fleece!(@object $enc $all_ok () ($($rest)*) ($($rest)*));
    };

    // Next value is a map plus values.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: {$($map:tt)*} , $($rest:tt)*) $copy:tt) => {
        $all_ok &= $crate::ffi::FLEncoder_WriteKey($enc.as_ptr(), ($($key)+).into());
        $all_ok &= $crate::ffi::FLEncoder_BeginDict($enc.as_ptr(), 0);
        $crate::fleece!(@object $enc $all_ok () ($($map)*)  ($($map)*));
        $all_ok &= $crate::ffi::FLEncoder_EndDict($enc.as_ptr());
        $crate::fleece!(@object $enc $all_ok () ($($rest)*) ($($rest)*));
    };

    // Next value is a map and this is last.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: {$($map:tt)*}) $copy:tt) => {
        $all_ok &= $crate::ffi::FLEncoder_WriteKey($enc.as_ptr(), ($($key)+).into());
        $all_ok &= $crate::ffi::FLEncoder_BeginDict($enc.as_ptr(), 0);
        $crate::fleece!(@object $enc $all_ok () ($($map)*)  ($($map)*));
        $all_ok &= $crate::ffi::FLEncoder_EndDict($enc.as_ptr());
    };

    // Next value is an expression followed by comma.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::fleece!(@object $enc $all_ok [$($key)+] ($value) , $($rest)*);
    };

    // Last value is an expression with no trailing comma.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::fleece!(@object $enc $all_ok [$($key)+] ($value));
    };

    // Missing value for last entry. Trigger a reasonable error message.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) (:) $copy:tt) => {
        // "unexpected end of macro invocation"
        $crate::fleece!();
    };

    // Missing colon and value for last entry. Trigger a reasonable error
    // message.
    (@object $enc:ident $all_ok:ident ($($key:tt)+) () $copy:tt) => {
        // "unexpected end of macro invocation"
        $crate::fleece!();
    };

    // Misplaced colon. Trigger a reasonable error message.
    (@object $enc:ident $all_ok:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        // Takes no arguments so "no rules expected the token `:`".
        fleece_unexpected!($colon);
    };

    // Found a comma inside a key. Trigger a reasonable error message.
    (@object $enc:ident $all_ok:ident ($($key:tt)*) (, $($rest:tt)*) ($comma:tt $($copy:tt)*)) => {
        // Takes no arguments so "no rules expected the token `,`".
        fleece_unexpected!($comma);
    };

    // Key is fully parenthesized. This avoids clippy double_parens false
    // positives because the parenthesization may be necessary here.
    (@object $enc:ident $all_ok:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        $crate::fleece!(@object $enc $all_ok ($key) (: $($rest)*) (: $($rest)*));
    };

    // Refuse to absorb colon token into key expression.
    (@object $enc:ident $all_ok:ident ($($key:tt)*) (: $($unexpected:tt)+) $copy:tt) => {
        fleece_expect_expr_comma!($($unexpected)+);
    };

    // Munch a token into the current key.
    (@object $enc:ident $all_ok:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::fleece!(@object $enc $all_ok ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    //////////////////////////////////////////////////////////////////////////
    // The main implementation.
    //
    // Must be invoked as: fleece!($($json)+)
    //////////////////////////////////////////////////////////////////////////

    ({}) => { unsafe {
        match ::std::ptr::NonNull::new($crate::ffi::FLEncoder_New()) {
            Some(enc) => {
                let mut all_ok = true;
                all_ok &= $crate::ffi::FLEncoder_BeginDict(enc.as_ptr(), 0);
                all_ok &= $crate::ffi::FLEncoder_EndDict(enc.as_ptr());
                let mut err = $crate::ffi::FLError::kFLNoError;
                let data = $crate::ffi::FLEncoder_Finish(enc.as_ptr(), &mut err);
                $crate::ffi::FLEncoder_Free(enc.as_ptr());
                if all_ok && !data.is_empty() {
                    Ok(data)
                } else {
                    Err(err.into())
                }
            }
            None => Err($crate::Error::Fleece($crate::ffi::FLError::kFLMemoryError)),
        }
    }};


    ({ $($tt:tt)+ }) => {{
        unsafe {
            match ::std::ptr::NonNull::new($crate::ffi::FLEncoder_New()) {
                Some(enc) => {
                    let mut all_ok = true;
                    all_ok &= $crate::ffi::FLEncoder_BeginDict(enc.as_ptr(), 0);
                    $crate::fleece!(@object enc all_ok () ($($tt)+) ($($tt)+));
                    all_ok &= $crate::ffi::FLEncoder_EndDict(enc.as_ptr());
                    let mut err = $crate::ffi::FLError::kFLNoError;
                    let data = $crate::ffi::FLEncoder_Finish(enc.as_ptr(), &mut err);
                    $crate::ffi::FLEncoder_Free(enc.as_ptr());
                    if all_ok && !data.is_empty() {
                        Ok(data)
                    } else {
                        Err(err.into())
                    }
                }
                None => Err($crate::Error::Fleece($crate::ffi::FLError::kFLMemoryError)),
            }
        }
    }};
}

#[macro_export]
#[doc(hidden)]
macro_rules! fleece_unexpected {
    () => {};
}

#[macro_export]
#[doc(hidden)]
macro_rules! count_tts {
    () => {0usize};
    ($_head:tt , $($tail:tt)*) => {1usize + count_tts!($($tail)*)};
    ($item:tt) => {1usize};
}
