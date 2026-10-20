//! MPEG-TS timestamps and wall-clock formatting.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const WRAP: i64 = 1 << 33;

/// Turns 33-bit 90 kHz PTS/DTS values into a monotonic 64-bit timeline.
///
/// Every value is taken as the nearest point to the previous one, so the
/// timeline survives the wrap every 26.5 hours and small steps backwards
/// (B-frames, audio slightly behind video).
#[derive(Debug, Default, Clone, Copy)]
pub struct Unwrapper {
    last: Option<(u64, i64)>,
}

impl Unwrapper {
    pub fn unwrap(&mut self, raw: u64) -> i64 {
        let raw = raw & (WRAP as u64 - 1);
        let value = match self.last {
            None => raw as i64,
            Some((prev_raw, prev)) => prev + signed_delta(raw, prev_raw),
        };
        self.last = Some((raw, value));
        value
    }
}

/// The shortest signed distance from `from` to `to` on the 33-bit circle.
pub fn signed_delta(to: u64, from: u64) -> i64 {
    let d = (to as i64 - from as i64).rem_euclid(WRAP);
    if d >= WRAP / 2 { d - WRAP } else { d }
}

/// Converts ticks of `timescale` Hz to a duration (negative becomes zero).
pub fn ticks_to_duration(ticks: i64, timescale: u32) -> Duration {
    if ticks <= 0 || timescale == 0 {
        return Duration::ZERO;
    }
    let nanos = (ticks as u128 * 1_000_000_000) / u128::from(timescale);
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

/// Converts a duration to ticks of `timescale` Hz, rounding down.
pub fn duration_to_ticks(d: Duration, timescale: u32) -> i64 {
    (d.as_nanos() * u128::from(timescale) / 1_000_000_000) as i64
}

/// `2026-10-20T12:34:56.789Z`, as `EXT-X-PROGRAM-DATE-TIME` wants.
pub fn rfc3339_millis(t: SystemTime) -> String {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() as i64;
    let millis = since.subsec_millis();
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrapper_survives_the_33_bit_wrap() {
        let mut u = Unwrapper::default();
        let near_end = (1u64 << 33) - 3000;
        assert_eq!(u.unwrap(near_end), near_end as i64);
        assert_eq!(u.unwrap(0), near_end as i64 + 3000);
        assert_eq!(u.unwrap(3000), near_end as i64 + 6000);
        // A small step back (B-frame PTS) stays a small step back.
        assert_eq!(u.unwrap(1500), near_end as i64 + 4500);
    }

    #[test]
    fn signed_delta_handles_both_directions() {
        assert_eq!(signed_delta(10, 5), 5);
        assert_eq!(signed_delta(5, 10), -5);
        assert_eq!(signed_delta(0, (1 << 33) - 1), 1);
    }

    #[test]
    fn tick_conversions() {
        assert_eq!(ticks_to_duration(90_000, 90_000), Duration::from_secs(1));
        assert_eq!(ticks_to_duration(-5, 90_000), Duration::ZERO);
        assert_eq!(
            duration_to_ticks(Duration::from_millis(200), 90_000),
            18_000
        );
    }

    #[test]
    fn formats_program_date_time() {
        let t = UNIX_EPOCH + Duration::from_millis(1_792_500_000_123);
        assert_eq!(rfc3339_millis(t), "2026-10-20T12:40:00.123Z");
        assert_eq!(rfc3339_millis(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(rfc3339_millis(leap), "2000-02-29T00:00:00.000Z");
    }
}
