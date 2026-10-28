//! UTC timestamps without a date library: epoch seconds for JWT claims, and the
//! strict `YYYY-MM-DDTHH:MM:SS[.fff]Z` form the license file and the legacy
//! `ver` claim use.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn from_epoch_seconds(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}

/// Seconds since the epoch, held to the years 1 to 9999, and to what this
/// platform's clock holds (Windows' starts in 1601), rather than overflowing.
pub(crate) fn from_epoch_seconds_clamped(seconds: i64) -> SystemTime {
    const FIRST_SECOND_OF_1: i64 = -62_135_596_800;
    const LAST_SECOND_OF_9999: i64 = 253_402_300_799;
    const FIRST_SECOND_OF_1601: u64 = 11_644_473_600;

    let seconds = seconds.clamp(FIRST_SECOND_OF_1, LAST_SECOND_OF_9999);
    if seconds >= 0 {
        return UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64)).unwrap_or(UNIX_EPOCH);
    }
    UNIX_EPOCH
        .checked_sub(Duration::from_secs(seconds.unsigned_abs()))
        .or_else(|| UNIX_EPOCH.checked_sub(Duration::from_secs(FIRST_SECOND_OF_1601)))
        .unwrap_or(UNIX_EPOCH)
}

pub(crate) fn to_epoch_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        // Floor, so a time a fraction of a second before the epoch is -1, not 0.
        Err(before) => {
            let before = before.duration();
            let whole = before.as_secs() as i64;
            if before.subsec_nanos() > 0 { -whole - 1 } else { -whole }
        }
    }
}

/// How long ago `time` was, or zero if it is in the future.
pub(crate) fn age(time: SystemTime) -> Duration {
    SystemTime::now().duration_since(time).unwrap_or(Duration::ZERO)
}

// Howard Hinnant's days_from_civil / civil_from_days, valid far beyond any
// timestamp a license will carry.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvalidTimestamp;

impl std::fmt::Display for InvalidTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invalid ISO-8601 timestamp")
    }
}

/// Parse an explicit-UTC ISO-8601 timestamp. Offsets other than `Z`, missing
/// seconds and impossible dates are all refused rather than guessed at.
pub(crate) fn parse_iso8601_utc(value: &str) -> Result<SystemTime, InvalidTimestamp> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return Err(InvalidTimestamp);
    }

    let digits = |offset: usize, count: usize| -> Result<u32, InvalidTimestamp> {
        let slice = bytes.get(offset..offset + count).ok_or(InvalidTimestamp)?;
        slice.iter().try_fold(0u32, |acc, byte| {
            if byte.is_ascii_digit() { Ok(acc * 10 + u32::from(byte - b'0')) } else { Err(InvalidTimestamp) }
        })
    };
    let expect = |offset: usize, expected: u8| {
        if bytes.get(offset) == Some(&expected) { Ok(()) } else { Err(InvalidTimestamp) }
    };

    let year = i64::from(digits(0, 4)?);
    expect(4, b'-')?;
    let month = digits(5, 2)?;
    expect(7, b'-')?;
    let day = digits(8, 2)?;
    expect(10, b'T')?;
    let hour = digits(11, 2)?;
    expect(13, b':')?;
    let minute = digits(14, 2)?;
    expect(16, b':')?;
    let second = digits(17, 2)?;

    let mut cursor = 19;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return Err(InvalidTimestamp);
        }
    }
    if bytes.get(cursor) != Some(&b'Z') || cursor + 1 != bytes.len() {
        return Err(InvalidTimestamp);
    }

    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(InvalidTimestamp);
    }

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    Ok(from_epoch_seconds(seconds))
}

/// A time in one of the API's JSON answers, where .NET writes it with a `Z`, with
/// an offset, or with neither for a time it holds without a kind. The API keeps
/// every time in UTC, so one without a zone is read as UTC. License tokens never
/// take this path: their times are epoch seconds.
pub(crate) fn parse_api_timestamp(value: &str) -> Result<SystemTime, InvalidTimestamp> {
    // Date, time and any fraction run up to the zone designator.
    let bytes = value.as_bytes();
    let mut end = 19;
    if bytes.len() < end {
        return Err(InvalidTimestamp);
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    if !value.is_char_boundary(end) {
        return Err(InvalidTimestamp);
    }
    let (local, zone) = value.split_at(end);
    if zone.is_empty() || zone == "Z" {
        return parse_iso8601_utc(&format!("{local}Z"));
    }
    let zone = zone.as_bytes();
    if zone.len() == 6 && matches!(zone[0], b'+' | b'-') && zone[3] == b':' {
        let two_digits = |at: usize| -> Result<u64, InvalidTimestamp> {
            match (zone[at], zone[at + 1]) {
                (tens @ b'0'..=b'9', ones @ b'0'..=b'9') => Ok(u64::from(tens - b'0') * 10 + u64::from(ones - b'0')),
                _ => Err(InvalidTimestamp),
            }
        };
        let (hours, minutes) = (two_digits(1)?, two_digits(4)?);
        if hours > 23 || minutes > 59 {
            return Err(InvalidTimestamp);
        }
        let offset = Duration::from_secs(hours * 3_600 + minutes * 60);
        let utc = parse_iso8601_utc(&format!("{local}Z"))?;
        let moved = if zone[0] == b'+' { utc.checked_sub(offset) } else { utc.checked_add(offset) };
        return moved.ok_or(InvalidTimestamp);
    }
    Err(InvalidTimestamp)
}

/// Format as `YYYY-MM-DDTHH:MM:SSZ`, truncated to the second.
pub(crate) fn format_iso8601_utc(time: SystemTime) -> String {
    let seconds = to_epoch_seconds(time);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", of_day / 3_600, (of_day % 3_600) / 60, of_day % 60)
}

/// Serde adapters for a [`SystemTime`] as the license file writes one:
/// `YYYY-MM-DDTHH:MM:SSZ`, in UTC. For types of your own that carry a license's
/// times, as `#[serde(with = "moonbase::serde_iso8601")]`, or
/// `moonbase::serde_iso8601::option` for an `Option<SystemTime>`.
pub mod serde_iso8601 {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::SystemTime;

    /// Write a time as `YYYY-MM-DDTHH:MM:SSZ`, truncated to the second.
    pub fn serialize<S: Serializer>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::format_iso8601_utc(*time))
    }

    /// Read a time written with an explicit `Z`; offsets and impossible dates are
    /// refused.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_iso8601_utc(&text).map_err(serde::de::Error::custom)
    }

    /// The same, for an `Option<SystemTime>`: `None` is `null`.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};
        use std::time::SystemTime;

        pub fn serialize<S: Serializer>(time: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error> {
            match time {
                Some(time) => serializer.serialize_str(&crate::time::format_iso8601_utc(*time)),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<SystemTime>, D::Error> {
            Option::<String>::deserialize(deserializer)?
                .map(|text| crate::time::parse_iso8601_utc(&text).map_err(serde::de::Error::custom))
                .transpose()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_epoch_and_leap_days() {
        for text in [
            "1970-01-01T00:00:00Z",
            "2000-02-29T12:34:56Z",
            "2024-12-31T23:59:59Z",
            "2038-01-19T03:14:08Z",
            "1969-12-31T23:59:59Z",
        ] {
            let parsed = parse_iso8601_utc(text).unwrap();
            assert_eq!(format_iso8601_utc(parsed), text);
        }
        assert_eq!(to_epoch_seconds(parse_iso8601_utc("2000-02-29T12:34:56Z").unwrap()), 951_827_696);
    }

    #[test]
    fn accepts_only_explicit_utc() {
        assert!(parse_iso8601_utc("2024-01-02T03:04:05.123Z").is_ok());
        for bad in [
            "2024-01-02T03:04:05",
            "2024-01-02T03:04:05+00:00",
            "2024-01-02 03:04:05Z",
            "2024-01-02T03:04:05.Z",
            "2024-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2024-13-01T00:00:00Z",
            "2024-01-01T24:00:00Z",
            "2024-01-01T00:00:60Z",
            "2024-01-01T00:00:00Zjunk",
        ] {
            assert!(parse_iso8601_utc(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn api_timestamps_are_all_read_as_utc() {
        let utc = parse_iso8601_utc("2027-02-01T12:30:00Z").unwrap();
        for text in [
            "2027-02-01T12:30:00Z",
            "2027-02-01T12:30:00",
            "2027-02-01T12:30:00.1234567",
            "2027-02-01T12:30:00.5Z",
            "2027-02-01T14:30:00+02:00",
            "2027-02-01T07:00:00-05:30",
            "2027-02-01T12:30:00+00:00",
        ] {
            assert_eq!(parse_api_timestamp(text), Ok(utc), "{text}");
        }
        for bad in [
            "",
            "2027-02-01",
            "2027-02-01T12:30:00+0200",
            "2027-02-01T12:30:00 UTC",
            "2027-02-01T12:30:00+24:00",
            "2027-02-30T12:30:00Z",
            "next year",
        ] {
            assert!(parse_api_timestamp(bad).is_err(), "{bad} should not parse");
        }
    }
}
