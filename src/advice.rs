//! The server's own `Retry-After` advice — **both** RFC 9110 forms.

use core::time::Duration;

/// The upstream's advice about when to come back, as it was actually stated.
///
/// Both forms are kept distinct rather than eagerly normalised to a `Duration`,
/// because normalising requires a clock and a primitive that reads the clock
/// cannot be tested deterministically. Resolve with [`RetryAdvice::delay_from`],
/// supplying your own notion of now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAdvice {
    /// The **delta-seconds** form (RFC 9110 §10.2.3): `Retry-After: 120`.
    After(Duration),
    /// The **HTTP-date** form: `Retry-After: Sun, 06 Nov 1994 08:49:37 GMT`,
    /// carried as seconds since the Unix epoch.
    At { unix_secs: u64 },
}

impl RetryAdvice {
    /// Resolve to a delay, given the caller's notion of the current time as
    /// seconds since the Unix epoch.
    ///
    /// A date already in the past yields [`Duration::ZERO`] — the advice is
    /// satisfied, retry now — rather than underflowing.
    #[must_use]
    pub const fn delay_from(self, now_unix_secs: u64) -> Duration {
        match self {
            Self::After(d) => d,
            Self::At { unix_secs } => Duration::from_secs(unix_secs.saturating_sub(now_unix_secs)),
        }
    }

    /// Resolve against the system clock.
    ///
    /// Prefer [`RetryAdvice::delay_from`] wherever the caller already has a
    /// clock seam — this method exists for call sites that genuinely do not.
    /// A system clock before the Unix epoch yields the advice unresolved
    /// against zero rather than panicking.
    #[must_use]
    pub fn delay_now(self) -> Duration {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.delay_from(now)
    }
}

/// Parse a `Retry-After` header value.
///
/// Accepts both forms RFC 9110 §10.2.3 defines:
///
/// - **delta-seconds** — `120`
/// - **HTTP-date** (IMF-fixdate) — `Sun, 06 Nov 1994 08:49:37 GMT`
///
/// Returns `None` for anything else, which callers must treat as *no advice
/// given* and fall back to their own backoff.
///
/// ── ★ WHY THE DATE FORM IS HERE AND NOT IN `todoku` ───────────────────────
///
/// `todoku::parse_retry_after` parsed only delta-seconds, and said so honestly,
/// giving this reason: *"doing so needs an RFC 9110 IMF-fixdate parser, and
/// pulling a date crate here would ripple through `Cargo.gen.lock`."*
///
/// That limit was stated in terms of our own dependency policy, not in terms of
/// the world — and a limit phrased that way is ours to dissolve. IMF-fixdate is
/// a fixed ASCII grammar with no locale and no timezone arithmetic (it is
/// always GMT), so converting one to a Unix timestamp needs integer arithmetic
/// and nothing else. No date crate, no dependency, no lock churn.
///
/// The cost of the gap was real: a `503` or `429` carrying the date form was
/// read fleet-wide as *no advice at all*, so every consumer silently discarded
/// the one number the server actually volunteered and guessed instead.
///
/// **Honest limit:** only IMF-fixdate is accepted. RFC 9110 says a recipient
/// *should* also accept the obsolete rfc850 (`Sunday, 06-Nov-94 08:49:37 GMT`)
/// and asctime (`Sun Nov  6 08:49:37 1994`) forms; those return `None` here.
/// That is a narrower claim than "RFC compliant" and is stated rather than
/// implied.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<RetryAdvice> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    // delta-seconds first: it is what nearly every upstream sends, and it is
    // unambiguous (a bare run of digits is never a date).
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse::<u64>().ok().map(|s| RetryAdvice::After(Duration::from_secs(s)));
    }

    parse_imf_fixdate(value).map(|unix_secs| RetryAdvice::At { unix_secs })
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` → seconds since the Unix epoch.
fn parse_imf_fixdate(value: &str) -> Option<u64> {
    // Tokenising rather than fixed-offset slicing: the grammar mandates a fixed
    // width, but real servers do emit a single-digit day, and rejecting those
    // would discard advice we can plainly read.
    let mut parts = value.split_whitespace();
    let _weekday = parts.next()?; // "Sun," — the day name is redundant with the date
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_from_name(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    // The zone is mandatory and must be GMT; anything else is not IMF-fixdate
    // and guessing an offset would be worse than declining.
    if !parts.next()?.eq_ignore_ascii_case("GMT") || parts.next().is_some() {
        return None;
    }

    let mut hms = time.split(':');
    let hour: u64 = hms.next()?.parse().ok()?;
    let min: u64 = hms.next()?.parse().ok()?;
    let sec: u64 = hms.next()?.parse().ok()?;
    if hms.next().is_some() || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    if day == 0 || day > days_in_month(year, month) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    if days < 0 {
        // Before 1970. A `Retry-After` in 1969 is not advice we can express as
        // an epoch offset, and it is certainly in the past — decline rather
        // than wrap.
        return None;
    }
    let secs = u64::try_from(days).ok()? * 86_400 + hour * 3_600 + min * 60 + sec;
    Some(secs)
}

fn month_from_name(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(name))
        .map(|i| u32::try_from(i).unwrap_or(0) + 1)
}

const fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian date.
///
/// Howard Hinnant's `days_from_civil` — integer only, no lookup tables, correct
/// for the whole representable range. Chosen over a hand-rolled
/// accumulate-the-years loop because that shape gets leap centuries wrong and
/// the bug only appears in years divisible by 100.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400; // [0, 399]
    let mp = (month + 9) % 12; // Mar = 0
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{RetryAdvice, days_from_civil, parse_retry_after};
    use core::time::Duration;

    #[test]
    fn delta_seconds_form() {
        assert_eq!(
            parse_retry_after("120"),
            Some(RetryAdvice::After(Duration::from_secs(120)))
        );
        // Whitespace is normal in a header value.
        assert_eq!(
            parse_retry_after("  30  "),
            Some(RetryAdvice::After(Duration::from_secs(30)))
        );
        // Zero is valid advice: come back immediately.
        assert_eq!(
            parse_retry_after("0"),
            Some(RetryAdvice::After(Duration::ZERO))
        );
    }

    /// The exact example from RFC 9110 §10.2.3, against an independently
    /// computed epoch value. This is the discriminating test: an off-by-one in
    /// the civil-days arithmetic changes this number.
    #[test]
    fn http_date_form_matches_the_rfc_example() {
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(RetryAdvice::At {
                unix_secs: 784_111_777
            })
        );
    }

    #[test]
    fn http_date_form_tolerates_a_single_digit_day() {
        assert_eq!(
            parse_retry_after("Sun, 6 Nov 1994 08:49:37 GMT"),
            Some(RetryAdvice::At {
                unix_secs: 784_111_777
            })
        );
    }

    #[test]
    fn rejected_shapes_yield_no_advice() {
        for bad in [
            "",
            "   ",
            "soon",
            "-5",                              // negative delta-seconds
            "1.5",                             // fractional
            "Sun, 06 Nov 1994 08:49:37 PST",   // non-GMT zone: do not guess
            "Sun, 06 Nov 1994 08:49:37",       // no zone
            "Sun, 06 Xxx 1994 08:49:37 GMT",   // bad month
            "Sun, 31 Feb 1994 08:49:37 GMT",   // day out of range for month
            "Sun, 00 Nov 1994 08:49:37 GMT",   // day zero
            "Sun, 06 Nov 1994 25:49:37 GMT",   // hour out of range
            "Sunday, 06-Nov-94 08:49:37 GMT",  // obsolete rfc850 — stated limit
            "Sun Nov  6 08:49:37 1994",        // obsolete asctime — stated limit
            "Sun, 06 Nov 1969 08:49:37 GMT",   // before the epoch
        ] {
            assert_eq!(parse_retry_after(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn leap_day_and_leap_century_are_correct() {
        // 2000 is a leap year (divisible by 400) — Feb 29 exists.
        assert!(parse_retry_after("Tue, 29 Feb 2000 00:00:00 GMT").is_some());
        // 1900 was NOT a leap year (divisible by 100, not 400) — but it also
        // predates the epoch, so assert the arithmetic directly instead.
        assert_eq!(days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28), 2);
        assert_eq!(days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28), 1);
        // 2100 will not be a leap year either.
        assert_eq!(days_from_civil(2100, 3, 1) - days_from_civil(2100, 2, 28), 1);
        assert_eq!(parse_retry_after("Mon, 29 Feb 2100 00:00:00 GMT"), None);
    }

    #[test]
    fn the_epoch_itself_is_day_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(
            parse_retry_after("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(RetryAdvice::At { unix_secs: 0 })
        );
    }

    #[test]
    fn a_past_date_resolves_to_zero_rather_than_underflowing() {
        let advice = RetryAdvice::At {
            unix_secs: 784_111_777,
        };
        assert_eq!(advice.delay_from(900_000_000), Duration::ZERO);
        assert_eq!(advice.delay_from(784_111_677), Duration::from_secs(100));
    }

    #[test]
    fn delta_seconds_ignores_the_clock() {
        let advice = RetryAdvice::After(Duration::from_secs(42));
        assert_eq!(advice.delay_from(0), Duration::from_secs(42));
        assert_eq!(advice.delay_from(u64::MAX), Duration::from_secs(42));
    }
}
