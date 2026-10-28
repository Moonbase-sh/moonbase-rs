//! `IOPlatformUUID` straight from IOKit. No `ioreg` subprocess, so it works inside
//! the App Sandbox and under the hardened runtime, where spawning one does not.

use std::ffi::{c_char, c_void};

use crate::fingerprint_spec::normalize_platform_uuid;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFTypeID = usize;
type CFIndex = isize;
type CFStringEncoding = u32;
type Boolean = u8;
type MachPort = u32;
type IoObject = MachPort;

const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;
// MACH_PORT_NULL selects the default main port and compiles against every SDK,
// unlike kIOMainPortDefault (macOS 12+) or the deprecated kIOMasterPortDefault.
const MACH_PORT_NULL: MachPort = 0;
const IO_OBJECT_NULL: IoObject = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    // Consumes one reference to `matching`.
    fn IOServiceGetMatchingService(main_port: MachPort, matching: CFMutableDictionaryRef) -> IoObject;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: IoObject) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: CFAllocatorRef,
        text: *const c_char,
        encoding: CFStringEncoding,
    ) -> CFStringRef;
    fn CFGetTypeID(value: CFTypeRef) -> CFTypeID;
    fn CFStringGetTypeID() -> CFTypeID;
    fn CFStringGetCString(
        value: CFStringRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: CFStringEncoding,
    ) -> Boolean;
    fn CFRelease(value: CFTypeRef);
}

/// The platform UUID with hyphens removed and uppercased, or empty when IOKit
/// does not provide one.
pub(super) fn read_io_platform_uuid() -> String {
    // SAFETY: plain IOKit/CoreFoundation calls following the create/release rules;
    // every object created here is released before returning.
    unsafe {
        let service =
            IOServiceGetMatchingService(MACH_PORT_NULL, IOServiceMatching(c"IOPlatformExpertDevice".as_ptr()));
        if service == IO_OBJECT_NULL {
            return String::new();
        }

        let key = CFStringCreateWithCString(std::ptr::null(), c"IOPlatformUUID".as_ptr(), K_CF_STRING_ENCODING_UTF8);
        let property = if key.is_null() {
            std::ptr::null()
        } else {
            let property = IORegistryEntryCreateCFProperty(service, key, std::ptr::null(), 0);
            CFRelease(key);
            property
        };
        IOObjectRelease(service);

        if property.is_null() {
            return String::new();
        }

        let mut uuid = String::new();
        if CFGetTypeID(property) == CFStringGetTypeID() {
            let mut buffer = [0 as c_char; 128];
            if CFStringGetCString(property, buffer.as_mut_ptr(), buffer.len() as CFIndex, K_CF_STRING_ENCODING_UTF8)
                != 0
            {
                uuid = std::ffi::CStr::from_ptr(buffer.as_ptr()).to_string_lossy().into_owned();
            }
        }
        CFRelease(property);

        normalize_platform_uuid(&uuid)
    }
}
