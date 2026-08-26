//! Unix-time conversions shared by scan/materialize.
//!
//! The manifest stores mtime as signed seconds plus normalized nanoseconds
//! (`docs/store-format.md`). Host APIs hand us `SystemTime`, which cannot
//! express pre-1970 instants through `duration_since` without the error
//! branch; these helpers do the round trip exactly, including pre-1970
//! values, and are pure enough to unit-test identically on every platform.

use std::time::{Duration, SystemTime};

/// `(sec, nsec)` with `0 <= nsec < 1_000_000_000`; negative seconds are
/// timespec-style pre-1970 instants (negative sec, POSITIVE nsec).
pub fn split_unix(t: SystemTime) -> (i64, u32) {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            if d.subsec_nanos() == 0 {
                (-(d.as_secs() as i64), 0)
            } else {
                (-(d.as_secs() as i64) - 1, 1_000_000_000 - d.subsec_nanos())
            }
        }
    }
}

/// Inverse of [`split_unix`].
pub fn join_unix(sec: i64, nsec: u32) -> SystemTime {
    let total = i128::from(sec) * 1_000_000_000 + i128::from(nsec);
    if total >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(total as u64)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_nanos((-total) as u64)
    }
}

/// Break unix seconds into (year, month, day, hour, minute, second), UTC.
pub fn civil_utc(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let d = i64::from(d);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYYMMDD-HHMMSS` form used inside conflict file names.
pub fn fmt_compact(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Format unix seconds into `HH:MM:SS` UTC.
pub fn fmt_time_utc(secs: i64) -> String {
    let (_, _, _, h, mi, s) = civil_utc(secs);
    format!("{h:02}:{mi:02}:{s:02}")
}

/// Formats the current time as `HH:MM:SS`.
pub fn current_time_str() -> String {
    let (secs, _) = now_unix();
    fmt_time_utc(secs)
}

/// RFC 3339 UTC with second precision.
pub fn fmt_rfc3339(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Parse standard RFC 3339 UTC timestamp into unix seconds.
pub fn parse_rfc3339_to_unix(ts: &str) -> Option<u64> {
    let ts = ts.trim();
    if ts.len() < 20 {
        return None;
    }
    let parts: Vec<&str> = ts.split('T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year: i64 = date_parts[0].parse().ok()?;
    let month: u32 = date_parts[1].parse().ok()?;
    let day: u32 = date_parts[2].parse().ok()?;

    let time_str = parts[1].trim_end_matches('Z');
    let time_parts: Vec<&str> = time_str.split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].split('.').next()?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    let total_secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    if total_secs >= 0 {
        Some(total_secs as u64)
    } else {
        None
    }
}

/// Current wall clock as (unix seconds, nanoseconds).
pub fn now_unix() -> (i64, u32) {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            (-(d.as_secs() as i64), 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Windows SystemTime is FILETIME-backed (100ns units), so finer digits
    // cannot survive a join→split round trip there. Quantize expectations to
    // the platform's clock granularity; unix keeps full fidelity.
    const NS_GRAN: u32 = if cfg!(windows) { 100 } else { 1 };
    fn q(nsec: u32) -> u32 {
        nsec / NS_GRAN * NS_GRAN
    }

    #[test]
    fn round_trips_post_epoch() {
        for (sec, nsec) in [
            (0i64, 0u32),
            (1_700_000_000, q(123_456_789)),
            (86_400, q(999_999_999)),
        ] {
            assert_eq!(split_unix(join_unix(sec, nsec)), (sec, nsec));
        }
    }

    #[test]
    fn round_trips_pre_epoch_timespec_style() {
        for (sec, nsec) in [
            (-1i64, 0u32),
            (-1, q(999_999_999)),
            (-50_000, q(5)),
            (-9_999, q(1)),
        ] {
            assert_eq!(
                split_unix(join_unix(sec, nsec)),
                (sec, nsec),
                "({sec},{nsec})"
            );
        }
    }

    #[test]
    fn epoch_minus_half_second_is_minus_one_second_plus_half() {
        let t = SystemTime::UNIX_EPOCH - Duration::from_millis(500);
        assert_eq!(split_unix(t), (-1, 500_000_000));
    }

    #[test]
    fn roundtrip_rfc3339() {
        let now_sec = 1_787_574_896;
        let formatted = fmt_rfc3339(now_sec);
        let parsed = parse_rfc3339_to_unix(&formatted).expect("parsed");
        assert_eq!(parsed, now_sec as u64);
    }

    #[test]
    fn civil_round_trips_over_a_wide_range() {
        for sec in [0, 86400, 1_700_000_000, 2_000_000_000] {
            let (y, mo, d, h, mi, s) = civil_utc(sec);
            let days = days_from_civil(y, mo, d);
            let reconstructed = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
            assert_eq!(reconstructed, sec);
        }
    }
}
