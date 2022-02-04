#[macro_export]
macro_rules! fleece {
    ({ $($key:tt : $value:tt),* }) => {{
        unsafe {
            match ::std::ptr::NonNull::new($crate::ffi::FLEncoder_New()) {
                Some(enc) => {
                    let mut all_ok = true;
                    all_ok &= $crate::ffi::FLEncoder_BeginDict(enc.as_ptr(), 0);
                    $(
                        all_ok &= $crate::ffi::FLEncoder_WriteKey(enc.as_ptr(), $key.into());
                        all_ok &= $crate::ffi::FLEncoder_WriteString(enc.as_ptr(), $value.into());
                    )*
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
