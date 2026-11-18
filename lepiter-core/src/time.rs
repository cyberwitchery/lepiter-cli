use std::cmp::Ordering;
use std::fmt;

use jiff::fmt::temporal::{DateTimePrinter, Pieces};
use jiff::tz::Offset;
use serde::{Serialize, Serializer};

/// An instant with the utc offset it was written in.
///
/// Equality and ordering compare the instant only, so two timestamps that
/// name the same moment in different offsets are equal.
#[derive(Debug, Clone, Copy)]
pub struct Timestamp {
    instant: jiff::Timestamp,
    offset: Offset,
}

impl Timestamp {
    /// The current time, in utc.
    pub fn now() -> Self {
        Self {
            instant: jiff::Timestamp::now(),
            offset: Offset::UTC,
        }
    }

    /// Parses an rfc 3339 timestamp such as `2024-01-01T00:00:00+02:00`,
    /// keeping its offset. A date without a time, a missing offset or a
    /// time zone annotation is `None`.
    pub fn parse_rfc3339(s: &str) -> Option<Self> {
        let pieces = Pieces::parse(s).ok()?;
        if pieces.time_zone_annotation().is_some() {
            return None;
        }
        let offset = pieces.to_numeric_offset()?;
        let datetime = pieces.date().to_datetime(pieces.time()?);
        let instant = offset.to_timestamp(datetime).ok()?;
        Some(Self { instant, offset })
    }

    /// The instant, independent of offset.
    pub fn instant(&self) -> jiff::Timestamp {
        self.instant
    }

    /// The offset the timestamp was written in.
    pub fn offset(&self) -> Offset {
        self.offset
    }

    /// Formats as rfc 3339 in the original offset, with `+00:00` for utc and
    /// fractional seconds in groups of three digits.
    pub fn to_rfc3339(&self) -> String {
        self.format(false)
    }

    fn format(&self, zulu: bool) -> String {
        let nanos = self.instant.subsec_nanosecond();
        let precision = if nanos == 0 {
            0
        } else if nanos % 1_000_000 == 0 {
            3
        } else if nanos % 1_000 == 0 {
            6
        } else {
            9
        };
        let printer = DateTimePrinter::new().precision(Some(precision));
        if zulu && self.offset == Offset::UTC {
            printer.timestamp_to_string(&self.instant)
        } else {
            printer.timestamp_with_offset_to_string(&self.instant, self.offset)
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl PartialEq for Timestamp {
    fn eq(&self, other: &Self) -> bool {
        self.instant == other.instant
    }
}

impl Eq for Timestamp {}

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.instant.cmp(&other.instant)
    }
}

/// Serializes as rfc 3339, with `Z` for utc.
impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.format(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rfc(s: &str) -> String {
        Timestamp::parse_rfc3339(s).unwrap().to_rfc3339()
    }

    fn json(s: &str) -> String {
        serde_json::to_string(&Timestamp::parse_rfc3339(s).unwrap()).unwrap()
    }

    #[test]
    fn keeps_offset() {
        assert_eq!(
            rfc("2024-01-01T00:00:00+02:00"),
            "2024-01-01T00:00:00+02:00"
        );
        assert_eq!(
            rfc("2024-01-01T00:00:00-05:30"),
            "2024-01-01T00:00:00-05:30"
        );
    }

    #[test]
    fn utc_prints_numeric_offset() {
        assert_eq!(rfc("2024-01-01T00:00:00Z"), "2024-01-01T00:00:00+00:00");
        assert_eq!(
            rfc("2024-01-01T00:00:00+00:00"),
            "2024-01-01T00:00:00+00:00"
        );
        assert_eq!(
            rfc("2024-01-01T00:00:00-00:00"),
            "2024-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn fractional_seconds_in_groups_of_three() {
        assert_eq!(
            rfc("2021-03-31T11:21:44.43+02:00"),
            "2021-03-31T11:21:44.430+02:00"
        );
        assert_eq!(
            rfc("2021-03-31T11:21:44.436658+02:00"),
            "2021-03-31T11:21:44.436658+02:00"
        );
        assert_eq!(
            rfc("2021-03-31T11:21:44.4300001-05:30"),
            "2021-03-31T11:21:44.430000100-05:30"
        );
    }

    #[test]
    fn accepts_lowercase_and_space_separator() {
        assert_eq!(rfc("2024-01-01t00:00:00z"), "2024-01-01T00:00:00+00:00");
        assert_eq!(rfc("2024-01-01 00:00:00Z"), "2024-01-01T00:00:00+00:00");
    }

    #[test]
    fn rejects_non_timestamps() {
        for s in [
            "not-a-date",
            "2024-01-01",
            "2024-01-01T00:00:00",
            "2024-01-01T00:00:00+02:00[Europe/Berlin]",
        ] {
            assert!(Timestamp::parse_rfc3339(s).is_none(), "{s}");
        }
    }

    #[test]
    fn serializes_utc_as_z() {
        assert_eq!(
            json("2024-01-01T00:00:00+00:00"),
            "\"2024-01-01T00:00:00Z\""
        );
        assert_eq!(
            json("2021-03-31T11:21:44.43+02:00"),
            "\"2021-03-31T11:21:44.430+02:00\""
        );
    }

    #[test]
    fn compares_instants() {
        let a = Timestamp::parse_rfc3339("2024-01-01T02:00:00+02:00").unwrap();
        let b = Timestamp::parse_rfc3339("2024-01-01T00:00:00Z").unwrap();
        let c = Timestamp::parse_rfc3339("2024-01-01T00:00:01Z").unwrap();
        assert_eq!(a, b);
        assert!(b < c);
    }
}
