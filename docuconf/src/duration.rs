//! Durations: parsed with `humantime` (as `humantime_serde` does when
//! figment deserializes the field), written in canonical Go form.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::decl::DurationEncoding;

/// Parses a duration the way the host does. `humantime` accepts every Go
/// duration the platform renders (`1m30s`, `720h`, `250ms`, `90us`), so the
/// contract records the `go` encoding.
/// Surrounding whitespace is rejected, since values are never trimmed.
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    if s.trim() != s {
        return Err("is not a duration such as \"1m30s\" (it has surrounding whitespace)".into());
    }
    humantime::parse_duration(s).map_err(|_| "is not a duration such as \"1m30s\"".to_string())
}

/// Parses a duration in one of the wire encodings of SPEC §5.
pub(crate) fn parse_encoded(enc: DurationEncoding, s: &str) -> Result<Duration, String> {
    match enc {
        DurationEncoding::Go => parse_duration(s),
        DurationEncoding::Iso8601 => parse_iso8601(s)
            .ok_or_else(|| "is not an ISO 8601 duration such as \"PT90S\"".to_string()),
        DurationEncoding::Seconds => parse_seconds(s)
            .ok_or_else(|| "is not a number of seconds such as \"90\" or \"1.5\"".to_string()),
        DurationEncoding::Timespan => parse_timespan(s).ok_or_else(|| {
            "is not a TimeSpan such as \"00:01:30\" or \"1.02:03:04.5\"".to_string()
        }),
    }
}

/// `whole` units of `unit` nanoseconds plus a decimal fraction of a unit.
fn nanos(whole: &str, frac: Option<&str>, unit: u128) -> Option<u128> {
    let mut n = whole.parse::<u128>().ok()?.checked_mul(unit)?;
    if let Some(f) = frac {
        // Digits finer than a nanosecond cannot be represented.
        let digits = f.len() as u32;
        let scale = 10u128.checked_pow(digits)?;
        let part = f.parse::<u128>().ok()?.checked_mul(unit)?;
        if part % scale != 0 {
            return None;
        }
        n = n.checked_add(part / scale)?;
    }
    Some(n)
}

fn from_nanos(n: u128) -> Option<Duration> {
    let secs = u64::try_from(n / 1_000_000_000).ok()?;
    Some(Duration::new(secs, (n % 1_000_000_000) as u32))
}

const NS_SEC: u128 = 1_000_000_000;

static ISO8601: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^P(?:([0-9]+)D)?(?:T(?:([0-9]+)H)?(?:([0-9]+)M)?(?:([0-9]+)(?:[.,]([0-9]+))?S)?)?$",
    )
    .unwrap()
});

/// ISO 8601 durations with days, hours, minutes and (fractional) seconds,
/// as java.time.Duration and pydantic read them: `PT90S`, `PT1.5S`,
/// `P1DT2H`. Years, months and weeks have no fixed length and are rejected.
fn parse_iso8601(s: &str) -> Option<Duration> {
    let c = ISO8601.captures(s)?;
    // "P" alone, or a "T" with nothing after it, is not a duration.
    if s == "P" || s.ends_with('T') {
        return None;
    }
    let mut n: u128 = 0;
    for (i, unit) in [(1, 86_400 * NS_SEC), (2, 3_600 * NS_SEC), (3, 60 * NS_SEC)] {
        if let Some(m) = c.get(i) {
            n = n.checked_add(nanos(m.as_str(), None, unit)?)?;
        }
    }
    if let Some(m) = c.get(4) {
        n = n.checked_add(nanos(m.as_str(), c.get(5).map(|f| f.as_str()), NS_SEC)?)?;
    }
    from_nanos(n)
}

static SECONDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([0-9]+)(?:\.([0-9]+))?$").unwrap());

/// A non-negative decimal number of seconds: `90`, `0.25`.
fn parse_seconds(s: &str) -> Option<Duration> {
    let c = SECONDS.captures(s)?;
    from_nanos(nanos(&c[1], c.get(2).map(|m| m.as_str()), NS_SEC)?)
}

static TIMESPAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:([0-9]+)\.)?([0-9]{1,2}):([0-9]{2}):([0-9]{2})(?:\.([0-9]{1,7}))?$").unwrap()
});

/// .NET `TimeSpan` in its constant form, `[d.]hh:mm:ss[.fffffff]`.
fn parse_timespan(s: &str) -> Option<Duration> {
    let c = TIMESPAN.captures(s)?;
    let h: u128 = c[2].parse().ok()?;
    let m: u128 = c[3].parse().ok()?;
    let sec: u128 = c[4].parse().ok()?;
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    let days = match c.get(1) {
        Some(d) => nanos(d.as_str(), None, 86_400 * NS_SEC)?,
        None => 0,
    };
    let n = days
        .checked_add(h * 3_600 * NS_SEC + m * 60 * NS_SEC)?
        .checked_add(nanos(&c[4], c.get(5).map(|f| f.as_str()), NS_SEC)?)?;
    from_nanos(n)
}

/// Formats a duration in canonical Go form: `1h30m`, `1m30s`, `1s500ms`,
/// `0s`. Each unit appears at most once, largest first; zero units are
/// left out.
pub fn format_go(d: Duration) -> String {
    let mut n = d.as_nanos();
    if n == 0 {
        return "0s".into();
    }
    let mut out = String::new();
    for (unit, name) in [
        (3_600_000_000_000u128, "h"),
        (60_000_000_000, "m"),
        (1_000_000_000, "s"),
        (1_000_000, "ms"),
        (1_000, "us"),
        (1, "ns"),
    ] {
        let q = n / unit;
        if q > 0 {
            out.push_str(&format!("{q}{name}"));
            n -= q * unit;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_form() {
        assert_eq!(format_go(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_go(Duration::from_secs(5400)), "1h30m");
        assert_eq!(format_go(Duration::from_secs(720 * 3600)), "720h");
        assert_eq!(format_go(Duration::from_millis(1500)), "1s500ms");
        assert_eq!(format_go(Duration::from_micros(90)), "90us");
        assert_eq!(format_go(Duration::ZERO), "0s");
    }

    #[test]
    fn parses_go_forms() {
        for (s, want) in [
            ("1m30s", 90_000),
            ("1h30m", 5_400_000),
            ("250ms", 250),
            ("720h", 2_592_000_000),
        ] {
            assert_eq!(parse_duration(s).unwrap().as_millis(), want, "{s}");
        }
        assert_eq!(parse_duration("90us").unwrap(), Duration::from_micros(90));
        assert_eq!(parse_duration("5ns").unwrap(), Duration::from_nanos(5));
        // Go accepts fractions too.
        assert_eq!(parse_duration("1.5h").unwrap().as_secs(), 5400);
        assert!(parse_duration(" 1s").is_err());
        assert!(parse_duration("1s\n").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn parses_every_encoding() {
        use DurationEncoding::*;
        let ok = |e, s: &str| parse_encoded(e, s).unwrap_or_else(|m| panic!("{s}: {m}"));
        assert_eq!(ok(Iso8601, "PT90S"), Duration::from_secs(90));
        assert_eq!(ok(Iso8601, "PT1.5S"), Duration::from_millis(1500));
        assert_eq!(ok(Iso8601, "PT0.001S"), Duration::from_millis(1));
        assert_eq!(ok(Iso8601, "P1DT2H3M4S"), Duration::from_secs(93_784));
        assert_eq!(ok(Iso8601, "PT0S"), Duration::ZERO);
        for bad in ["P", "PT", "1m30s", "PT1H30", "P1Y", "PT-1S", " PT1S", "90"] {
            assert!(parse_encoded(Iso8601, bad).is_err(), "{bad}");
        }
        assert_eq!(ok(Seconds, "90"), Duration::from_secs(90));
        assert_eq!(ok(Seconds, "0.25"), Duration::from_millis(250));
        assert_eq!(ok(Seconds, "0"), Duration::ZERO);
        for bad in ["90s", "", "-1", "1e3", ".5", "1.", " 1"] {
            assert!(parse_encoded(Seconds, bad).is_err(), "{bad}");
        }
        assert_eq!(ok(Timespan, "00:01:30"), Duration::from_secs(90));
        assert_eq!(
            ok(Timespan, "1.02:03:04.5"),
            Duration::from_millis(93_784_500)
        );
        assert_eq!(ok(Timespan, "2.00:00:00"), Duration::from_secs(172_800));
        for bad in [
            "1m30s",
            "24:00:00",
            "00:60:00",
            "00:00:60",
            "1:30",
            "00:01:30.12345678",
        ] {
            assert!(parse_encoded(Timespan, bad).is_err(), "{bad}");
        }
        assert_eq!(ok(Go, "1h2m3s4ms"), Duration::from_millis(3_723_004));
        assert!(parse_encoded(Go, "PT90S").is_err());
    }
}
