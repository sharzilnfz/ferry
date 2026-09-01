use std::time::{Duration, SystemTime};

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

pub fn join_unix(sec: i64, nsec: u32) -> SystemTime {
    let total = i128::from(sec) * 1_000_000_000 + i128::from(nsec);
    if total >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(total as u64)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_nanos((-total) as u64)
    }
}

pub fn civil_utc(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let dt =
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    (
        i64::from(dt.year()),
        u32::from(dt.month() as u8),
        u32::from(dt.day()),
        u32::from(dt.hour()),
        u32::from(dt.minute()),
        u32::from(dt.second()),
    )
}

pub fn fmt_compact(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_utc(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

pub fn fmt_time_utc(secs: i64) -> String {
    let (_, _, _, h, mi, s) = civil_utc(secs);
    format!("{h:02}:{mi:02}:{s:02}")
}

pub fn current_time_str() -> String {
    let (secs, _) = now_unix();
    fmt_time_utc(secs)
}

pub fn fmt_rfc3339(secs: i64) -> String {
    let dt =
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let format = time::format_description::well_known::Rfc3339;
    dt.format(&format)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub fn parse_rfc3339_to_unix(ts: &str) -> Option<u64> {
    let format = time::format_description::well_known::Rfc3339;
    let dt = time::OffsetDateTime::parse(ts.trim(), &format).ok()?;
    let secs = dt.unix_timestamp();
    if secs >= 0 {
        Some(secs as u64)
    } else {
        None
    }
}

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

    fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
        let Ok(month) = time::Month::try_from(m as u8) else {
            return 0;
        };
        let Ok(date) = time::Date::from_calendar_date(y as i32, month, d as u8) else {
            return 0;
        };
        date.midnight().assume_utc().unix_timestamp() / 86_400
    }

    #[test]
    fn civil_round_trips_over_a_wide_range() {
        for sec in [0, 86400, 1_700_000_000, 2_000_000_000] {
            let (y, mo, d, h, mi, s) = civil_utc(sec);
            let days = days_from_civil(y, mo, d);
            let reconstructed =
                days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
            assert_eq!(reconstructed, sec);
        }
    }
}
