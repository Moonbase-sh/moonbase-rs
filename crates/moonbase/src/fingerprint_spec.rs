//! The Moonbase device fingerprint specification, version 2.
//!
//! See `FINGERPRINT_SPEC.md` at the repository root for the normative text and
//! `tests/vectors/fingerprint-vectors.json` for the conformance suite. When the
//! prose and the vectors disagree, the vectors win: they are what every SDK can
//! actually execute.
//!
//! A license token carries a `sig` claim equal to the device id, and each SDK
//! recomputes that id locally on every offline validation. This module is
//! byte-exact and deterministic, so given the same platform tag and parameters it
//! produces the same device id as `@moonbase.sh/licensing` and `moonbase-cpp`.
//!
//! Everything here is pure: nothing reads the machine, so the Windows SMBIOS parser
//! and the macOS ioreg parser are tested on every platform. The reads themselves
//! live in [`MoonbaseDeviceIdResolver`](crate::MoonbaseDeviceIdResolver).

use unicode_normalization::UnicodeNormalization;

use crate::error::{Error, Result};

/// First line of the hashed material. Always carries the same version number as
/// the device id stamp.
pub const PREFIX: &str = "moonbase:fingerprint:v2";

/// Spec version, present in both the material prefix and the id stamp.
pub const VERSION: u32 = 2;

/// Longest permitted canonical value, in characters.
pub const MAX_VALUE_LENGTH: usize = 128;

/// Where a device id's identity came from, and therefore what it may be compared
/// to. Each non-hardware source carries its own stamp so the limitation travels
/// with the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceIdSource {
    /// Hardware identity, comparable across every conforming SDK. `mbd2_`
    Identity,
    /// The opt-in host-name fallback. `mbd2n_`
    DeviceName,
    /// Stable for this device within one platform-defined scope (the iOS vendor,
    /// the Android signing key), and not comparable across scopes. `mbd2s_`
    Scoped,
}

impl DeviceIdSource {
    /// The stamp's source tag.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Identity => "",
            Self::DeviceName => "n",
            Self::Scoped => "s",
        }
    }

    /// The source a tag stands for, or `None` for a tag this SDK does not define.
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "" => Some(Self::Identity),
            "n" => Some(Self::DeviceName),
            "s" => Some(Self::Scoped),
            _ => None,
        }
    }
}

/// A `(name, value)` identity parameter.
pub type Parameter = (String, String);

/// What a device id stamp says about how it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdStamp {
    /// Fingerprint spec version that produced the digest.
    pub version: u32,
    /// The literal source tag: `""`, `"n"`, `"s"`, or one a newer SDK introduced.
    pub source_tag: String,
    /// What `source_tag` means, or `None` when this SDK does not define that tag.
    pub source: Option<DeviceIdSource>,
    /// The 64-character lowercase-hex SHA-256.
    pub digest: String,
}

// Exactly these parameters describe the individual machine. Everything else a
// platform collects is model-level: vendor, product and board names are identical
// across every unit of a product line, so a material built only from those would
// give every machine of that model the same device id.
const IDENTIFYING_PARAMS: [&str; 7] = [
    "ioPlatformUuid",
    "machineId",
    "systemUuid",
    "baseboardSerialNumber",
    // Scoped sources: identifying within the platform's own scope, which is all
    // those platforms allow.
    "identifierForVendor",
    "androidId",
    "deviceName",
];

// OEM filler that is not really a value. Compared case-insensitively, and only
// for identifying parameters: a descriptive field reading "Default string" still
// describes the model, whereas a serial number reading it is not a serial number.
const NOT_PROGRAMMED_VALUES: [&str; 15] = [
    "to be filled by o.e.m.",
    "to be filled by oem",
    "default string",
    "system serial number",
    "base board serial number",
    "chassis serial number",
    "not specified",
    "not applicable",
    "not available",
    "none",
    "unknown",
    "invalid",
    "n/a",
    "0123456789",
    "uninitialized",
];

// Rejected for androidId only. Keyed by parameter on purpose: widening the shared
// list would change the device id of a machine that reports the same string as,
// say, a baseboard serial, which the spec forbids without a version bump.
const REJECTED_ANDROID_IDS: [&str; 1] = [
    // A real ANDROID_ID shared by a large batch of 2010-era devices whose
    // ro.serialno was unset. Valid hex, so the format rule cannot catch it.
    "9774d56d682e549c",
];

fn is_lowercase_hex(value: &str) -> bool {
    value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Canonicalize a raw value: NFC, then printable ASCII only, then capped at 128
/// characters, then space-trimmed at both ends. In that order.
///
/// Interior spaces are preserved and nothing else is altered: no case folding, no
/// reordering. Dropping everything outside U+0020..U+007E makes the material
/// grammar unambiguous (a value cannot contain an LF to forge an extra line),
/// makes the decoding of raw firmware strings irrelevant, and absorbs the trailing
/// newline that sysfs reads carry.
pub fn canonicalize_value(value: &str) -> String {
    let mut printable: String = value.nfc().filter(|c| (' '..='~').contains(c)).collect();
    // Pure ASCII by now, so characters and bytes are the same thing.
    printable.truncate(MAX_VALUE_LENGTH);
    printable.trim_matches(' ').to_owned()
}

/// The platform tag for the OS this binary runs on.
///
/// Mac Catalyst (`*-apple-ios-macabi`) counts as macOS: it can read both
/// `identifierForVendor` and IOKit, and hardware identity wins, so it agrees with
/// an Electron or web SDK on the same Mac. Every other Apple platform maps to
/// `ios`.
pub const fn platform_tag() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "macos") || cfg!(all(target_os = "ios", target_abi = "macabi")) {
        "mac"
    } else if cfg!(any(target_os = "ios", target_os = "tvos", target_os = "watchos", target_os = "visionos")) {
        "ios"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly")) {
        "bsd"
    } else {
        "unknown"
    }
}

/// The identifying parameter names, in spec order.
pub fn identifying_param_names() -> Vec<&'static str> {
    IDENTIFYING_PARAMS.to_vec()
}

pub fn is_identifying_param(name: &str) -> bool {
    IDENTIFYING_PARAMS.contains(&name)
}

/// Is this canonical value OEM filler rather than a real identifier?
///
/// Includes any value that is entirely `0`s or entirely `f`/`F`s: a blank UUID
/// field or a zeroed `machine-id`.
pub fn is_not_programmed(canonical_value: &str) -> bool {
    if canonical_value.is_empty() {
        return false;
    }
    if canonical_value.bytes().all(|byte| byte == b'0')
        || canonical_value.bytes().all(|byte| byte == b'f' || byte == b'F')
    {
        return true;
    }
    NOT_PROGRAMMED_VALUES.iter().any(|filler| canonical_value.eq_ignore_ascii_case(filler))
}

/// Does this canonical value look like a real Android SSAID?
///
/// `^[0-9a-f]{1,16}$` plus the placeholder rule. That makes the classic mistake of
/// reading the `Settings.Secure.ANDROID_ID` *field* (the key name `"android_id"`,
/// identical on every device) mechanically impossible: it is not hex.
pub fn is_valid_android_id(canonical_value: &str) -> bool {
    !canonical_value.is_empty()
        && canonical_value.len() <= 16
        && is_lowercase_hex(canonical_value)
        && !is_not_programmed(canonical_value)
        && !REJECTED_ANDROID_IDS.iter().any(|rejected| canonical_value.eq_ignore_ascii_case(rejected))
}

/// Can this identifying value stand for the machine?
pub fn is_usable_identity(name: &str, canonical_value: &str) -> bool {
    if is_not_programmed(canonical_value) {
        return false;
    }
    if name == "androidId" {
        return is_valid_android_id(canonical_value);
    }
    true
}

/// Canonicalize every value and drop the pairs that do not survive.
///
/// A pair is dropped when its canonical value is empty, or when its name is
/// identifying and the value is an unprogrammed placeholder. Both filters run
/// before the duplicate check, so two entries sharing a name where one is empty
/// are not a duplicate.
///
/// Fails with [`Error::DuplicateFingerprintParameter`] when two surviving pairs
/// share a name.
pub fn canonicalize_params<N: AsRef<str>, V: AsRef<str>>(params: &[(N, V)]) -> Result<Vec<Parameter>> {
    let mut kept: Vec<Parameter> = Vec::with_capacity(params.len());
    for (name, value) in params {
        let name = name.as_ref();
        let value = canonicalize_value(value.as_ref());
        if value.is_empty() || (is_identifying_param(name) && !is_usable_identity(name, &value)) {
            continue;
        }
        if kept.iter().any(|(kept_name, _)| kept_name == name) {
            return Err(Error::DuplicateFingerprintParameter(name.to_owned()));
        }
        kept.push((name.to_owned(), value));
    }
    Ok(kept)
}

/// Assemble the material to be hashed.
///
/// Lines are **joined** by a single LF, never terminated by one: the material does
/// not end with a newline.
///
/// Fails with [`Error::InsufficientDeviceIdentity`] when nothing survives
/// canonicalization, when nothing that survives identifies this individual
/// machine, or when the host-name fallback is attempted on iOS or Android.
pub fn build_fingerprint_material<N: AsRef<str>, V: AsRef<str>>(platform: &str, params: &[(N, V)]) -> Result<String> {
    let kept = canonicalize_params(params)?;
    if kept.is_empty() {
        return Err(Error::insufficient_identity(platform));
    }

    // Enforced here rather than in the resolver so it also binds a custom reader.
    // On these platforms the host name is a constant (since iOS 17 gethostname()
    // returns "localhost"), so accepting it would let one activation validate
    // across the whole install base.
    let scoped_platform = platform == "ios" || platform == "android";
    if scoped_platform && kept.iter().any(|(name, _)| name == "deviceName") {
        return Err(Error::InsufficientDeviceIdentity {
            platform: platform.to_owned(),
            reason: "the host-name fallback is not available on this platform, where the host name is the same \
                     on every device"
                .into(),
        });
    }

    if !kept.iter().any(|(name, _)| is_identifying_param(name)) {
        let names: Vec<&str> = kept.iter().map(|(name, _)| name.as_str()).collect();
        return Err(Error::InsufficientDeviceIdentity {
            platform: platform.to_owned(),
            reason: format!(
                "only model-level parameters could be read ({}), none of which identify this individual machine",
                names.join(", ")
            ),
        });
    }

    let mut material = String::from(PREFIX);
    material.push_str("\nplatform=");
    material.push_str(platform);
    for (name, value) in &kept {
        material.push('\n');
        material.push_str(name);
        material.push('=');
        material.push_str(value);
    }
    Ok(material)
}

/// Hash material into a bare digest: 64 lowercase hex characters of SHA-256.
pub fn fingerprint_digest(material: &str) -> String {
    crate::crypto::sha256_hex(material.as_bytes())
}

/// Prefix a digest with its version and source, producing the wire-form device id.
pub fn stamp_device_id(digest: &str, source: DeviceIdSource) -> String {
    format!("mbd{VERSION}{}_{digest}", source.tag())
}

/// Hash material and stamp it: the device id sent to Moonbase and stored in `sig`.
pub fn fingerprint_device_id(material: &str, source: DeviceIdSource) -> String {
    stamp_device_id(&fingerprint_digest(material), source)
}

/// Recover the version and source from a device id, or `None` if it is not a
/// Moonbase stamp.
///
/// Accepts `^mbd(\d+)([a-z]*)_([0-9a-f]{64})$`, so a source tag introduced by a
/// newer SDK still parses. Strict otherwise: uppercase hex, a truncated digest and
/// a missing separator all fail rather than being coerced.
pub fn parse_device_id_stamp(device_id: &str) -> Option<DeviceIdStamp> {
    let rest = device_id.strip_prefix("mbd")?;
    let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
    // At least one digit, and few enough that the value cannot overflow.
    if digit_count == 0 || digit_count > 9 {
        return None;
    }
    let (digits, rest) = rest.split_at(digit_count);
    let tag_length = rest.bytes().take_while(u8::is_ascii_lowercase).count();
    let (source_tag, rest) = rest.split_at(tag_length);
    let digest = rest.strip_prefix('_')?;
    if digest.len() != 64 || !is_lowercase_hex(digest) {
        return None;
    }
    Some(DeviceIdStamp {
        version: digits.parse().ok()?,
        source_tag: source_tag.to_owned(),
        source: DeviceIdSource::from_tag(source_tag),
        digest: digest.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Source parsers. Pure, so the platform readers stay thin and every one of these
// is tested on every platform.

/// Normalize a platform UUID for the material: hyphens removed, uppercased.
pub fn normalize_platform_uuid(raw: &str) -> String {
    raw.chars().filter(|c| *c != '-').map(|c| c.to_ascii_uppercase()).collect()
}

/// Extract `IOPlatformUUID` from `ioreg -rd1 -c IOPlatformExpertDevice` output.
///
/// Kept for consumers that already have ioreg output; the resolver reads IOKit
/// directly, which works inside the App Sandbox where spawning a process does not.
pub fn parse_ioreg_platform_uuid(ioreg_output: &str) -> String {
    const KEY: &str = "\"IOPlatformUUID\"";
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    let Some(at) = ioreg_output.find(KEY) else {
        return String::new();
    };
    let Some(rest) = ioreg_output[at + KEY.len()..].trim_start_matches(is_space).strip_prefix('=') else {
        return String::new();
    };
    let Some(rest) = rest.trim_start_matches(is_space).strip_prefix('"') else {
        return String::new();
    };
    match rest.find('"') {
        Some(end) if end > 0 => normalize_platform_uuid(&rest[..end]),
        _ => String::new(),
    }
}

/// Does this canonical value look like a real `machine-id(5)`?
pub fn is_valid_machine_id(canonical_value: &str) -> bool {
    canonical_value.len() == 32 && is_lowercase_hex(canonical_value) && !is_not_programmed(canonical_value)
}

/// Pick the first `machine-id` source holding a valid id.
///
/// Each source is validated before selection rather than taking the first
/// non-empty one: `/etc/machine-id` legitimately holds the literal marker
/// `uninitialized` in a golden image awaiting first boot, and every machine
/// deployed from that image reads the same marker.
pub fn select_machine_id<S: AsRef<str>>(sources: &[S]) -> String {
    sources
        .iter()
        .map(|source| canonicalize_value(source.as_ref()))
        .find(|candidate| is_valid_machine_id(candidate))
        .unwrap_or_default()
}

struct SmbiosStructure<'a> {
    kind: u8,
    /// The formatted area, indexed from the structure header (byte 0 = type).
    formatted: &'a [u8],
    /// A string-index field holding N maps to `strings[N - 1]`.
    strings: Vec<String>,
}

// Firmware strings are decoded as Latin-1, matching the reference SDK's
// Buffer.toString('latin1'). Canonicalization drops everything above U+007E anyway,
// but a UTF-8 decoder could let a combining mark in the firmware bytes annihilate
// the ASCII character before it, and a Latin-1 decoder cannot.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// Walk an SMBIOS structure table (with no leading `RawSMBIOSData` header).
fn parse_smbios_structures(data: &[u8]) -> Vec<SmbiosStructure<'_>> {
    let mut structures = Vec::new();
    let mut offset = 0usize;
    let size = data.len();

    while offset + 4 <= size {
        let kind = data[offset];
        let length = usize::from(data[offset + 1]);
        // A structure shorter than its own header, or one running off the end of
        // the table, means the table is malformed from here on.
        if length < 4 || offset + length > size {
            break;
        }

        let mut structure = SmbiosStructure { kind, formatted: &data[offset..offset + length], strings: Vec::new() };

        // The string table follows the formatted area: NUL-terminated strings
        // ending in a double NUL. A structure with no strings is just the double NUL.
        let mut cursor = offset + length;
        if cursor + 1 < size && data[cursor] == 0 && data[cursor + 1] == 0 {
            cursor += 2;
        } else {
            while cursor < size {
                let end = data[cursor..].iter().position(|&byte| byte == 0).map_or(size, |at| cursor + at);
                structure.strings.push(latin1(&data[cursor..end]));
                cursor = end + 1;
                if cursor < size && data[cursor] == 0 {
                    cursor += 1;
                    break;
                }
            }
        }

        structures.push(structure);
        if kind == 127 {
            // End-of-table.
            break;
        }
        offset = cursor;
    }

    structures
}

/// Resolve a string-index field. Index 0, or one past the table, means "no string".
///
/// Bounded by the structure's own length, not by the size of the table: older
/// SMBIOS 2.x structures are shorter than the current layout, and reading past the
/// formatted area would resolve a garbage index from the string pool.
fn resolve_smbios_string(structure: &SmbiosStructure<'_>, field_offset: usize) -> String {
    let Some(&index) = structure.formatted.get(field_offset) else {
        return String::new();
    };
    let index = usize::from(index);
    if index == 0 {
        return String::new();
    }
    structure.strings.get(index - 1).cloned().unwrap_or_default()
}

/// Format a 16-byte UUID field as uppercase hex: the raw bytes in order, with no
/// hyphens and deliberately **not** the SMBIOS-canonical little-endian swap of the
/// first three fields. All-`00` or all-`FF` means "not set" and yields nothing.
fn format_smbios_uuid(structure: &SmbiosStructure<'_>, field_offset: usize) -> String {
    let Some(bytes) = structure.formatted.get(field_offset..field_offset + 16) else {
        return String::new();
    };
    if bytes.iter().all(|&byte| byte == 0x00) || bytes.iter().all(|&byte| byte == 0xFF) {
        return String::new();
    }
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// Extract the Windows identity parameters from a raw SMBIOS structure table.
///
/// Takes the first type-1 (System) and the first type-2 (Baseboard) structure.
/// Type 4 (Processor) is deliberately not collected: its values are model-level,
/// and the number of type-4 structures tracks the vCPU count, so collecting them
/// would change the device id every time a VM is resized.
///
/// Parameters are emitted even when their value is empty. Describing the firmware
/// is this function's job; deciding what counts is canonicalization's.
pub fn parse_smbios_params(data: &[u8]) -> Vec<Parameter> {
    let mut params = Vec::new();
    if data.is_empty() {
        return params;
    }
    let structures = parse_smbios_structures(data);

    if let Some(system) = structures.iter().find(|structure| structure.kind == 1) {
        params.push(("systemManufacturer".into(), resolve_smbios_string(system, 0x04)));
        params.push(("systemProductName".into(), resolve_smbios_string(system, 0x05)));
        params.push(("systemUuid".into(), format_smbios_uuid(system, 0x08)));
    }
    if let Some(baseboard) = structures.iter().find(|structure| structure.kind == 2) {
        params.push(("baseboardManufacturer".into(), resolve_smbios_string(baseboard, 0x04)));
        params.push(("baseboardProduct".into(), resolve_smbios_string(baseboard, 0x05)));
        params.push(("baseboardSerialNumber".into(), resolve_smbios_string(baseboard, 0x07)));
    }
    params
}
