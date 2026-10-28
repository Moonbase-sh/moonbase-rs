//! The platform reads behind [`MoonbaseDeviceIdResolver`](crate::MoonbaseDeviceIdResolver).
//! Thin on purpose: everything that interprets what they read lives in
//! [`fingerprint_spec`](crate::fingerprint_spec), which is tested on every OS.

use crate::fingerprint_spec::Parameter;

#[cfg(any(target_os = "macos", all(target_os = "ios", target_abi = "macabi")))]
mod iokit;

#[cfg(all(
    any(target_os = "ios", target_os = "tvos", target_os = "watchos", target_os = "visionos"),
    not(target_abi = "macabi")
))]
mod uikit;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "android")]
pub mod android;

#[cfg(not(target_os = "windows"))]
mod unix_host_name;

/// The identity parameters for this platform, in spec order. Reads are
/// best-effort: an unreadable source yields an empty value, which
/// canonicalization then drops.
pub(crate) fn read_identity_params() -> Vec<Parameter> {
    #[cfg(any(target_os = "macos", all(target_os = "ios", target_abi = "macabi")))]
    {
        vec![("ioPlatformUuid".into(), iokit::read_io_platform_uuid())]
    }

    #[cfg(all(
        any(target_os = "ios", target_os = "tvos", target_os = "watchos", target_os = "visionos"),
        not(target_abi = "macabi")
    ))]
    {
        vec![("identifierForVendor".into(), uikit::read_identifier_for_vendor())]
    }

    #[cfg(target_os = "windows")]
    {
        crate::fingerprint_spec::parse_smbios_params(&windows::read_smbios_table())
    }

    #[cfg(target_os = "android")]
    {
        vec![("androidId".into(), android::read_android_id())]
    }

    #[cfg(target_os = "linux")]
    {
        use crate::fingerprint_spec::select_machine_id;

        // All five sources are world-readable files, so the result does not
        // depend on privilege, on any installed CLI, or on the locale.
        let read = |path: &str| {
            std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default()
        };
        let machine_id = select_machine_id(&[read("/etc/machine-id"), read("/var/lib/dbus/machine-id")]);
        vec![
            ("machineId".into(), machine_id),
            ("sysVendor".into(), read("/sys/class/dmi/id/sys_vendor")),
            ("productName".into(), read("/sys/class/dmi/id/product_name")),
            ("boardVendor".into(), read("/sys/class/dmi/id/board_vendor")),
            ("boardName".into(), read("/sys/class/dmi/id/board_name")),
        ]
    }

    // BSD and anything else: the spec defines no parameters, so these always
    // resolve to insufficient identity.
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_os = "windows",
        target_os = "android",
        target_os = "linux"
    )))]
    {
        Vec::new()
    }
}

/// The host name, with a trailing `.local` removed on macOS.
pub(crate) fn read_host_name() -> String {
    #[cfg(target_os = "windows")]
    {
        windows::read_host_name()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let name = unix_host_name::read();
        if cfg!(target_os = "macos") {
            return strip_local_suffix(name);
        }
        name
    }
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn strip_local_suffix(mut name: String) -> String {
    const SUFFIX: &str = ".local";
    let Some(cut) = name.len().checked_sub(SUFFIX.len()) else {
        return name;
    };
    if name.get(cut..).is_some_and(|tail| tail.eq_ignore_ascii_case(SUFFIX)) {
        name.truncate(cut);
    }
    name
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_a_trailing_local_in_any_case() {
        assert_eq!(super::strip_local_suffix("Studio.local".into()), "Studio");
        assert_eq!(super::strip_local_suffix("Studio.LOCAL".into()), "Studio");
        assert_eq!(super::strip_local_suffix("local".into()), "local");
        assert_eq!(super::strip_local_suffix("Studio.localdomain".into()), "Studio.localdomain");
    }
}
