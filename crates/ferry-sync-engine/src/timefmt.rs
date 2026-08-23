//! Fixed UTC time formatting for conflict names and report timestamps.
//!
//! Names and log lines must be reproducible, so this module formats unix
//! seconds with the civil-from-days algorithm (Hinnant) instead of pulling a
//! date library. Second precision everywhere; quarantine names use the
//! loser's mtime, report lines use the caller-supplied wall clock.

/// Break unix seconds into (year, month, day, hour, minute, second), UTC.
pub fn civil_utc(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// Inverse of the day part of [`civil_utc`] (days since 1970-01-01).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYYMMDD-HHMMSS` form used inside conflict file names.
pub fn fmt_compact(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// RFC 3339 UTC with second precision, used in JSONL report lines.
pub fn fmt_rfc3339(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Current wall clock as (unix seconds, nanoseconds).
pub fn now_unix() -> (i64, u32) {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
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

    #[test]
    fn known_instants_format_correctly() {
        // 1970-01-01T00:00:00Z
        assert_eq!(fmt_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_compact(0), "19700101-000000");
        // 2026-08-24T12:34:56Z == 1787608496 (checked via date -ur ...).
        assert_eq!(fmt_rfc3339(1_787_574_896), "2026-08-24T12:34:56Z");
        assert_eq!(fmt_compact(1_787_574_896), "20260824-123456");
        // Leap-year day: 2024-02-29T23:59:59Z.
        assert_eq!(fmt_rfc3339(1_709_251_199), "2024-02-29T23:59:59Z");
        // One second before the epoch stays pre-1970 without panicking.
        assert_eq!(fmt_rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn civil_round_trips_over_a_wide_range() {
        for secs in (-50_000_000..=200_000_000).step_by(997) {
            let (y, mo, d, h, mi, s) = civil_utc(secs);
            // Rebuild the day count from (y, m, d) and require equality.
            let yy = if mo <= 2 { y - 1 } else { y };
            let era = yy.div_euclid(400);
            let yoe = yy - era * 400;
            let mp = if mo > 2 { mo - 3 } else { mo + 9 } as i64;
            let doy = (153 * mp + 2) / 5 + d as i64 - 1;
            let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
            let days = era * 146_097 + doe - 719_468;
            assert_eq!(days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s as i64, secs);
        }
    }
}
