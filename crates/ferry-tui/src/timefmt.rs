//! UTC time calculation and formatting utilities for activity logs and peer tables.

/// Break unix seconds into `(year, month, day, hour, minute, second)`, UTC.
#[must_use]
pub fn civil_utc(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

/// Format unix seconds into `HH:MM:SS` UTC.
#[must_use]
pub fn fmt_time_utc(secs: i64) -> String {
    let (_, _, _, h, mi, s) = civil_utc(secs);
    format!("{h:02}:{mi:02}:{s:02}")
}

/// Format unix seconds into full RFC3339 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
#[must_use]
pub fn fmt_rfc3339(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Current system clock in seconds and nanoseconds.
#[must_use]
pub fn now_unix() -> (i64, u32) {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            (-(d.as_secs() as i64), 0)
        }
    }
}

/// Formats the current time as `HH:MM:SS`.
#[must_use]
pub fn current_time_str() -> String {
    let (secs, _) = now_unix();
    fmt_time_utc(secs)
}
