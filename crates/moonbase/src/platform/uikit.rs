//! `identifierForVendor`, reached through the Objective-C runtime's C API so no
//! Objective-C source or binding crate is needed. A [scoped](crate::fingerprint_spec::DeviceIdSource::Scoped)
//! identity: two apps from different vendors compute different ids by design.

use std::ffi::{CStr, c_char, c_void};

use crate::fingerprint_spec::normalize_platform_uuid;

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
}

// Load the framework that defines the device class, so objc_getClass finds it
// even in a binary that does not otherwise link it.
#[cfg_attr(not(target_os = "watchos"), link(name = "UIKit", kind = "framework"))]
#[cfg_attr(target_os = "watchos", link(name = "WatchKit", kind = "framework"))]
unsafe extern "C" {}

/// The uppercase, hyphen-free vendor UUID, or empty when iOS declines to provide
/// one. It does until the device is first unlocked after boot, so absence is
/// transient and surfaces as insufficient identity rather than as a constant.
pub(super) fn read_identifier_for_vendor() -> String {
    // SAFETY: objc_msgSend is called through the exact signature of each selector
    // (no arguments, object or C-string return), as the runtime requires.
    unsafe {
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_cstr: unsafe extern "C" fn(Id, Sel) -> *const c_char =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        // UIDevice everywhere except watchOS, which has WKInterfaceDevice.
        let mut device_class = objc_getClass(c"UIDevice".as_ptr());
        if device_class.is_null() {
            device_class = objc_getClass(c"WKInterfaceDevice".as_ptr());
        }
        if device_class.is_null() {
            return String::new();
        }

        let device = send_id(device_class, sel_registerName(c"currentDevice".as_ptr()));
        if device.is_null() {
            return String::new();
        }
        let uuid = send_id(device, sel_registerName(c"identifierForVendor".as_ptr()));
        if uuid.is_null() {
            return String::new();
        }
        let text = send_id(uuid, sel_registerName(c"UUIDString".as_ptr()));
        if text.is_null() {
            return String::new();
        }
        let utf8 = send_cstr(text, sel_registerName(c"UTF8String".as_ptr()));
        if utf8.is_null() {
            return String::new();
        }
        normalize_platform_uuid(&CStr::from_ptr(utf8).to_string_lossy())
    }
}
