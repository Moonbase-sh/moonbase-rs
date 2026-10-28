//! Just enough Semantic Versioning to decide whether the license's released
//! version (`p:rel`) is newer than the running app.

use std::cmp::Ordering;
use std::fmt;

/// A parsed `major.minor.patch[-prerelease]`. Build metadata is dropped, as SemVer
/// says it does not affect precedence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-joined identifiers; empty for a normal release.
    pub prerelease: String,
}

fn parse_uint(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl Semver {
    /// A tolerant parse: accepts a leading `v`/`V`, a missing minor or patch
    /// (`2.4` is 2.4.0), a `-prerelease` suffix and a `+build` suffix. `None` when
    /// the numeric core is malformed.
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        if text.is_empty() {
            return None;
        }
        let text = text.split_once('+').map_or(text, |(core, _)| core);
        let (core, prerelease) = text.split_once('-').unwrap_or((text, ""));

        let mut parts = core.split('.');
        let major = parse_uint(parts.next()?)?;
        let minor = parts.next().map(parse_uint).unwrap_or(Some(0))?;
        let patch = parts.next().map(parse_uint).unwrap_or(Some(0))?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch, prerelease: prerelease.to_owned() })
    }
}

impl Ord for Semver {
    /// SemVer §11 precedence.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| {
            match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
                // A release outranks any prerelease of the same core version.
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    let mut left = self.prerelease.split('.');
                    let mut right = other.prerelease.split('.');
                    loop {
                        match (left.next(), right.next()) {
                            (None, None) => return Ordering::Equal,
                            // More identifiers wins.
                            (None, Some(_)) => return Ordering::Less,
                            (Some(_), None) => return Ordering::Greater,
                            (Some(l), Some(r)) => {
                                let order = match (parse_uint(l), parse_uint(r)) {
                                    (Some(ln), Some(rn)) => ln.cmp(&rn),
                                    // Numeric identifiers rank below alphanumeric ones.
                                    (Some(_), None) => Ordering::Less,
                                    (None, Some(_)) => Ordering::Greater,
                                    (None, None) => l.cmp(r),
                                };
                                if order != Ordering::Equal {
                                    return order;
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}

impl PartialOrd for Semver {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Semver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.prerelease.is_empty() {
            write!(f, "-{}", self.prerelease)?;
        }
        Ok(())
    }
}

/// Read the version an app says it is, the way Moonbase does when it judges
/// whether a license covers it (see
/// [`License::covers_version`](crate::License::covers_version)).
///
/// Forgiving: an optional leading `v`, then two to four numbers separated by dots
/// (missing ones are 0, and a fourth is ignored, so an assembly version works),
/// with any `-pre-release`, `+build` or space-separated suffix dropped, so
/// `3.0.0-beta.2` is judged as 3.0.0 and the result's prerelease is always empty.
///
/// `None` when the version can't be read, which means it is not checked at all.
/// Stricter than [`Semver::parse`], which also takes a lone major (`3`).
///
/// ```
/// use moonbase::version::parse_app_version;
///
/// assert_eq!(parse_app_version("v3.0.0-beta.2").unwrap().to_string(), "3.0.0");
/// assert_eq!(parse_app_version("2.4.1.0").unwrap().to_string(), "2.4.1");
/// assert!(parse_app_version("nightly").is_none());
/// ```
pub fn parse_app_version(version: &str) -> Option<Semver> {
    // As in every other Moonbase SDK, so a version only the backend would read
    // never gets judged differently on the device.
    const MAX_PART: u64 = 9_999_999;

    let text = version.trim_ascii();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let text = text.find(['-', '+', ' ']).map_or(text, |suffix| &text[..suffix]);

    let parts: Vec<&str> = text.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    let mut numbers = [0u64; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        // Leading zeros are fine ("007"); only the value is capped.
        let number = part.trim_start_matches('0').parse::<u64>().unwrap_or(0);
        if part.trim_start_matches('0').len() > 7 || number > MAX_PART {
            return None;
        }
        if index < 3 {
            numbers[index] = number;
        }
    }
    Some(Semver { major: numbers[0], minor: numbers[1], patch: numbers[2], prerelease: String::new() })
}

/// True when `latest` is strictly newer than `current`. False when either fails
/// to parse, so a malformed value never prompts an update.
pub fn update_available(current: &str, latest: &str) -> bool {
    match (Semver::parse(current), Semver::parse(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(Semver::parse("v2.4").unwrap().to_string(), "2.4.0");
        assert_eq!(Semver::parse(" 1.2.3-beta.1+build.9 ").unwrap().to_string(), "1.2.3-beta.1");
        for bad in ["", "v", "1..2", "1.2.3.4", "a.b.c", "1.-2"] {
            assert!(Semver::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn orders_by_semver_precedence() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(Semver::parse(pair[0]) < Semver::parse(pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn app_versions_are_read_the_way_moonbase_reads_them() {
        let read = |text: &str| parse_app_version(text).map(|version| version.to_string());
        assert_eq!(read("2.4.1").as_deref(), Some("2.4.1"));
        assert_eq!(read("v3.0.0-beta.2").as_deref(), Some("3.0.0"));
        assert_eq!(read("2.4.1.0").as_deref(), Some("2.4.1"));
        assert_eq!(read("3.1").as_deref(), Some("3.1.0"));
        assert_eq!(read(" V2.0 ").as_deref(), Some("2.0.0"));
        assert_eq!(read("1.2.3+build.5").as_deref(), Some("1.2.3"));
        assert_eq!(read("1.2.3 (build 77)").as_deref(), Some("1.2.3"));
        assert_eq!(read("007.0").as_deref(), Some("7.0.0"));
        assert_eq!(read("9999999.0").as_deref(), Some("9999999.0.0"));
        for bad in
            ["", "   ", "nightly", "3", "1.2.3.4.5", "1..2", "1.x", "-1.0", "10000000.0", "1.99999999999999999999"]
        {
            assert!(parse_app_version(bad).is_none(), "{bad:?} should not read");
        }
        // Leading zeros never overflow anything: only the value is capped.
        assert_eq!(read("1.0000000000000000001").as_deref(), Some("1.1.0"));
    }

    #[test]
    fn update_is_available_only_for_a_strictly_newer_parseable_version() {
        assert!(update_available("1.0.0", "1.0.1"));
        assert!(!update_available("1.0.1", "1.0.1"));
        assert!(!update_available("1.0.1", "1.0.0"));
        assert!(!update_available("1.0.0", "garbage"));
        assert!(!update_available("", "1.0.0"));
    }
}
