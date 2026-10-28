//! Conformance suite for the Moonbase device fingerprint spec, driven by
//! `tests/vectors/fingerprint-vectors.json`, vendored verbatim from
//! `@moonbase.sh/licensing`. The spec prose is normative but the vectors are
//! decisive: they are what every SDK can actually execute.
//!
//! Everything here is pure, so these run identically on every OS, the Windows
//! SMBIOS parser included. What they cannot prove is that the platform *readers*
//! agree with the reference SDK on real hardware; the fingerprint-parity workflow
//! closes that gap.

use moonbase::Error;
use moonbase::fingerprint_spec::{self as fp, DeviceIdSource};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/fingerprint-vectors.json")).expect("the vector file is JSON")
}

fn source(name: &str) -> DeviceIdSource {
    match name {
        "identity" => DeviceIdSource::Identity,
        "deviceName" => DeviceIdSource::DeviceName,
        "scoped" => DeviceIdSource::Scoped,
        other => panic!("unknown source in vectors: {other}"),
    }
}

fn params(json: &Value) -> Vec<(String, String)> {
    json.as_array()
        .unwrap()
        .iter()
        .map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].as_str().unwrap().to_owned()))
        .collect()
}

fn description(vector: &Value) -> &str {
    vector["description"].as_str().unwrap_or("?")
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap()).collect()
}

#[test]
fn vectors_pin_the_spec_version_this_sdk_implements() {
    // The cross-SDK drift alarm: fails before any individual vector does.
    let vectors = vectors();
    assert_eq!(vectors["version"], fp::VERSION);
    assert_eq!(vectors["materialPrefix"], fp::PREFIX);

    let digest = "a".repeat(64);
    assert!(
        fp::stamp_device_id(&digest, DeviceIdSource::Identity).starts_with(vectors["stampPrefix"].as_str().unwrap())
    );
    for (name, tag) in vectors["sourceTags"].as_object().unwrap() {
        assert_eq!(
            fp::stamp_device_id(&digest, source(name)),
            format!("mbd{}{}_{digest}", fp::VERSION, tag.as_str().unwrap()),
            "source {name}"
        );
    }
}

#[test]
fn canonicalize_value_vectors() {
    for vector in vectors()["canonicalizeValue"].as_array().unwrap() {
        let input = vector["input"].as_str().unwrap();
        assert_eq!(
            fp::canonicalize_value(input),
            vector["expected"].as_str().unwrap(),
            "{}: {input:?}",
            description(vector)
        );
    }
}

#[test]
fn material_vectors() {
    for vector in vectors()["material"].as_array().unwrap() {
        let platform = vector["platform"].as_str().unwrap();
        let params = params(&vector["params"]);
        let material = fp::build_fingerprint_material(platform, &params)
            .unwrap_or_else(|error| panic!("{}: {error}", description(vector)));

        assert_eq!(material, vector["material"].as_str().unwrap(), "{}", description(vector));
        assert!(!material.ends_with('\n'));
        assert_eq!(fp::fingerprint_digest(&material), vector["digest"].as_str().unwrap());
        assert_eq!(
            fp::fingerprint_device_id(&material, source(vector["source"].as_str().unwrap())),
            vector["deviceId"].as_str().unwrap(),
            "{}",
            description(vector)
        );
    }
}

#[test]
fn material_error_vectors() {
    for vector in vectors()["materialErrors"].as_array().unwrap() {
        let result = fp::build_fingerprint_material(vector["platform"].as_str().unwrap(), &params(&vector["params"]));
        match (vector["error"].as_str().unwrap(), result) {
            ("InsufficientDeviceIdentity", Err(Error::InsufficientDeviceIdentity { .. })) => {}
            ("DuplicateParameter", Err(Error::DuplicateFingerprintParameter(_))) => {}
            (expected, other) => panic!("{}: expected {expected}, got {other:?}", description(vector)),
        }
    }
}

#[test]
fn smbios_vectors() {
    for vector in vectors()["smbios"].as_array().unwrap() {
        let table = decode_hex(vector["table"].as_str().unwrap());
        assert_eq!(fp::parse_smbios_params(&table), params(&vector["params"]), "{}", description(vector));
    }
}

#[test]
fn stamp_vectors() {
    for vector in vectors()["stamps"].as_array().unwrap() {
        let parsed = fp::parse_device_id_stamp(vector["deviceId"].as_str().unwrap());
        let expected = &vector["parsed"];
        match parsed {
            None => assert!(expected.is_null(), "{}: did not parse", description(vector)),
            Some(stamp) => {
                assert!(!expected.is_null(), "{}: parsed but should not", description(vector));
                assert_eq!(stamp.version, expected["version"].as_u64().unwrap() as u32);
                assert_eq!(stamp.source_tag, expected["sourceTag"].as_str().unwrap());
                assert_eq!(stamp.digest, expected["digest"].as_str().unwrap());
                assert_eq!(stamp.source, expected["source"].as_str().map(source), "{}", description(vector));
            }
        }
    }
}

#[test]
fn ioreg_output_is_parsed_and_normalized() {
    let output = r#"+-o J316sAP  <class IOPlatformExpertDevice, id 0x100000213>
    {
      "IOPlatformSerialNumber" = "C02XXXXXXX"
      "IOPlatformUUID" = "0123abcd-4567-89ef-0123-456789abcdef"
    }"#;
    assert_eq!(fp::parse_ioreg_platform_uuid(output), "0123ABCD456789EF0123456789ABCDEF");
    for broken in
        ["", r#""IOPlatformUUID""#, r#""IOPlatformUUID" = """#, r#""IOPlatformUUID" = "abc"#, r#""IOPlatformUUID" "x""#]
    {
        assert_eq!(fp::parse_ioreg_platform_uuid(broken), "", "{broken:?}");
    }
}

#[test]
fn each_machine_id_source_is_validated_before_it_is_selected() {
    let valid = "b08dfa6083e7567a1921a715000001fb";
    assert_eq!(fp::select_machine_id(&[format!("{valid}\n"), String::new()]), valid);
    // A golden image's marker, a zeroed id and an all-f id all fall through to D-Bus.
    for placeholder in ["uninitialized\n", "00000000000000000000000000000000", "ffffffffffffffffffffffffffffffff"] {
        assert_eq!(fp::select_machine_id(&[placeholder, valid]), valid, "{placeholder}");
    }
    assert_eq!(fp::select_machine_id(&["B08DFA6083E7567A1921A715000001FB", "short"]), "");
}

#[test]
fn android_ids_must_look_like_a_real_ssaid() {
    assert!(fp::is_valid_android_id("a1b2c3d4e5f60718"));
    // Pre-Oreo ids drop leading zeros.
    assert!(fp::is_valid_android_id("1b2c3d4e5f60718"));
    for bad in ["android_id", "A1B2C3D4E5F60718", "a1b2c3d4e5f607189", "9774d56d682e549c", "0000000000000000", ""] {
        assert!(!fp::is_valid_android_id(bad), "{bad}");
    }
}

#[test]
fn the_smbios_parser_never_panics_on_arbitrary_bytes() {
    // A tiny deterministic generator, so a failure reproduces.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..5000 {
        let length = (next() % 96) as usize;
        let mut bytes: Vec<u8> = (0..length).map(|_| next() as u8).collect();
        if let Some(first) = bytes.first_mut() {
            *first = [1, 2, 127][(next() % 3) as usize];
        }
        let _ = fp::parse_smbios_params(&bytes);
    }
}
