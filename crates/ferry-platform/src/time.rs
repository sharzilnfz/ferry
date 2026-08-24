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
    let total = sec as i128 * 1_000_000_000 + nsec as i128;
    if total >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(total as u64)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_nanos((-total) as u64)
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
}
