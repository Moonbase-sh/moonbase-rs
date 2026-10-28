//! RS256 verification and SHA-256, on `ring`.
//!
//! Key text is turned into DER by hand rather than by a PEM library, so a key that
//! lost its line breaks or picked up indentation on the way through a config file,
//! an XML attribute, a JSON string or an environment variable still loads.

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};

use crate::error::{Error, Result};

const STANDARD_INDIFFERENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

const URL_SAFE_INDIFFERENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Standard base64, skipping any whitespace inside the text.
pub(crate) fn base64_decode(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    STANDARD_INDIFFERENT.decode(compact)
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn base64url_decode(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_INDIFFERENT.decode(text)
}

/// Lowercase hex SHA-256. The formatting is part of the device fingerprint's
/// output, so it must never change.
pub(crate) fn sha256_hex(material: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = ring::digest::digest(&ring::digest::SHA256, material);
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0F)]));
    }
    out
}

fn unsupported_key() -> Error {
    Error::Configuration("Public key is not a supported RSA public key".into())
}

/// Strip the PEM armor, if any, and base64-decode to DER. The armor is found by
/// its markers rather than by line.
fn decode_key_bytes(key_material: &str) -> Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN";
    const END: &str = "-----END";
    const DASHES: &str = "-----";

    let body = match key_material.find(BEGIN) {
        None => key_material,
        Some(begin) => {
            // The label ("PUBLIC KEY", "RSA PUBLIC KEY") runs up to the next dashes.
            let label_end = key_material[begin + BEGIN.len()..]
                .find(DASHES)
                .map(|offset| begin + BEGIN.len() + offset)
                .ok_or_else(unsupported_key)?;
            let body = &key_material[label_end + DASHES.len()..];
            match body.find(END) {
                Some(end) => &body[..end],
                None => body,
            }
        }
    };
    base64_decode(body).map_err(|_| unsupported_key())
}

struct Tlv<'a> {
    tag: u8,
    content: &'a [u8],
}

/// Read one definite-length DER element off the front of `input`.
fn read_tlv<'a>(input: &mut &'a [u8]) -> Result<Tlv<'a>> {
    let (&tag, rest) = input.split_first().ok_or_else(unsupported_key)?;
    let (&first, mut rest) = rest.split_first().ok_or_else(unsupported_key)?;
    let length = if first & 0x80 == 0 {
        usize::from(first)
    } else {
        // Long form. RSA-2048 pushes the inner structures past 127 bytes, so this
        // is the common case, not a corner case.
        let count = usize::from(first & 0x7F);
        if count == 0 || count > std::mem::size_of::<usize>() || rest.len() < count {
            return Err(unsupported_key());
        }
        let (length_bytes, after) = rest.split_at(count);
        rest = after;
        length_bytes.iter().fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte))
    };
    if rest.len() < length {
        return Err(unsupported_key());
    }
    let (content, after) = rest.split_at(length);
    *input = after;
    Ok(Tlv { tag, content })
}

/// Normalize SPKI or PKCS#1 DER to the (modulus, exponent) pair.
fn rsa_components(der: &[u8]) -> Result<RsaPublicKeyComponents<Vec<u8>>> {
    let mut top = der;
    let outer = read_tlv(&mut top)?;
    if outer.tag != 0x30 {
        return Err(unsupported_key());
    }

    let mut inner = outer.content;
    let mut peek = inner;
    let first = read_tlv(&mut peek)?;

    let pkcs1: &[u8] = match first.tag {
        // INTEGER first: already PKCS#1 RSAPublicKey.
        0x02 => outer.content,
        // SEQUENCE first: SPKI, SEQUENCE { AlgorithmIdentifier, BIT STRING }.
        0x30 => {
            read_tlv(&mut inner)?;
            let bit_string = read_tlv(&mut inner)?;
            // The first content byte is the unused-bit count; the rest is the
            // embedded PKCS#1 DER.
            let (_, mut embedded) =
                bit_string.content.split_first().filter(|_| bit_string.tag == 0x03).ok_or_else(unsupported_key)?;
            let sequence = read_tlv(&mut embedded)?;
            if sequence.tag != 0x30 {
                return Err(unsupported_key());
            }
            sequence.content
        }
        _ => return Err(unsupported_key()),
    };

    let mut fields = pkcs1;
    let modulus = read_tlv(&mut fields)?;
    let exponent = read_tlv(&mut fields)?;
    if modulus.tag != 0x02 || exponent.tag != 0x02 {
        return Err(unsupported_key());
    }

    // Drop the 0x00 sign byte DER prepends when the high bit is set.
    let strip = |bytes: &[u8]| {
        let mut bytes = bytes;
        while bytes.len() > 1 && bytes[0] == 0 {
            bytes = &bytes[1..];
        }
        bytes.to_vec()
    };
    Ok(RsaPublicKeyComponents { n: strip(modulus.content), e: strip(exponent.content) })
}

/// An RSA public key that verifies RS256 signatures.
pub(crate) struct RsaPublicKey {
    components: RsaPublicKeyComponents<Vec<u8>>,
}

impl RsaPublicKey {
    /// Accepts PEM SPKI (`BEGIN PUBLIC KEY`), PEM PKCS#1 (`BEGIN RSA PUBLIC KEY`),
    /// and bare base64 of either DER encoding.
    pub(crate) fn parse(key_material: &str) -> Result<Self> {
        let der = decode_key_bytes(key_material)?;
        if der.is_empty() {
            return Err(unsupported_key());
        }
        Ok(Self { components: rsa_components(&der)? })
    }

    pub(crate) fn verify_rs256(&self, signing_input: &[u8], signature: &[u8]) -> Result<()> {
        self.components
            .verify(&RSA_PKCS1_2048_8192_SHA256, signing_input, signature)
            .map_err(|_| Error::LicenseInvalid("License token signature is not valid".into()))
    }
}

impl std::fmt::Debug for RsaPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RsaPublicKey").field("bits", &(self.components.n.len() * 8)).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_to_lowercase_hex() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn rejects_garbage_keys() {
        for key in ["", "not base64 at all!", "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----", "MAA="] {
            assert!(RsaPublicKey::parse(key).is_err(), "{key:?} should not parse");
        }
    }
}
