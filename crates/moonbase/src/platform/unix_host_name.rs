use std::ffi::{CStr, c_char, c_int};

unsafe extern "C" {
    fn gethostname(name: *mut c_char, len: usize) -> c_int;
}

/// `gethostname(2)`, as UTF-8. Empty when the call fails.
pub(super) fn read() -> String {
    let mut buffer = [0 as c_char; 256];
    // One byte short, so the result is NUL-terminated even when truncated.
    // SAFETY: the pointer and length describe a writable buffer we own.
    if unsafe { gethostname(buffer.as_mut_ptr(), buffer.len() - 1) } != 0 {
        return String::new();
    }
    // SAFETY: the last byte is never written, so the buffer holds a NUL.
    let name = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    name.to_string_lossy().into_owned()
}
