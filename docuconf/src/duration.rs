//! Durations: parsed with `humantime` (as `humantime_serde` does when
//! figment deserializes the field), written in canonical Go form.

use std::time::Duration;

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
}
