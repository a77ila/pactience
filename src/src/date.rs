//! Minimal UTC date <-> epoch conversion (Howard Hinnant's civil calendar
//! algorithms), avoiding a chrono/time dependency for two small functions.

/// Days since the Unix epoch for a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Epoch seconds for `YYYY-MM-DD HH:MM` interpreted as UTC.
pub fn epoch_from_date_time(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> i64 {
    days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60
}

/// `(year, month, day)` for the given epoch-second timestamp, UTC.
pub fn civil_from_epoch(epoch: i64) -> (i64, i64, i64) {
    let z = epoch.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format an epoch-second timestamp as `YYYY-MM-DD` (UTC).
pub fn format_date(epoch: i64) -> String {
    let (y, m, d) = civil_from_epoch(epoch);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_boundaries() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(epoch_from_date_time(1970, 1, 1, 0, 0), 0);
        // Verified against `date -u -d "2024-03-27 15:44:00" +%s`.
        assert_eq!(epoch_from_date_time(2024, 3, 27, 15, 44), 1_711_554_240);
    }

    #[test]
    fn round_trip() {
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (2024, 12, 31), (1900, 3, 1)] {
            let epoch = epoch_from_date_time(y, m, d, 0, 0);
            assert_eq!(civil_from_epoch(epoch), (y, m, d));
            assert_eq!(format_date(epoch), format!("{y:04}-{m:02}-{d:02}"));
        }
    }
}
