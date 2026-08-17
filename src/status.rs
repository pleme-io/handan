//! The status code itself, as a type rather than a bare `u16`.

/// An HTTP response status code.
///
/// A newtype rather than a bare `u16` for one reason that earned itself: every
/// consumer in the fleet held the code as a `u16` and then immediately
/// interpolated it into a string, because a `u16` offers no vocabulary for
/// asking what it *means*. The predicates below are that vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Status(u16);

impl Status {
    /// Wrap a raw code.
    #[must_use]
    pub const fn new(code: u16) -> Self {
        Self(code)
    }

    /// The raw code.
    #[must_use]
    pub const fn code(self) -> u16 {
        self.0
    }

    /// `2xx`.
    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 >= 200 && self.0 < 300
    }

    /// `4xx`.
    #[must_use]
    pub const fn is_client_error(self) -> bool {
        self.0 >= 400 && self.0 < 500
    }

    /// `5xx`.
    #[must_use]
    pub const fn is_server_error(self) -> bool {
        self.0 >= 500 && self.0 < 600
    }

    /// `429 Too Many Requests`.
    #[must_use]
    pub const fn is_rate_limited(self) -> bool {
        self.0 == 429
    }

    /// Whether an identical request, sent later, could plausibly succeed.
    ///
    /// ── ★ WHY THIS IS AN EXPLICIT SET AND NOT `is_server_error()` ─────────
    ///
    /// Two fleet crates derived this independently and **disagreed**, which is
    /// the divergence that extracting this crate surfaced:
    ///
    /// - `todoku`'s `RetryPolicy` used the explicit set `{429, 500, 502, 503, 504}`
    /// - a sibling content-sync crate's `is_transient` used `is_server_error()`,
    ///   i.e. every `5xx`
    ///
    /// The explicit set is correct and the all-`5xx` form is a (small) bug:
    /// `501 Not Implemented` and `505 HTTP Version Not Supported` are
    /// *permanent* refusals — the server is telling you it will never do this,
    /// so retrying is pure waste and, under a retry budget, it consumes
    /// attempts that a genuinely transient failure needed.
    ///
    /// `408 Request Timeout` is included on top of both originals: it is the
    /// one `4xx` whose entire meaning is "try again", and neither original
    /// covered it.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(self.0, 408 | 429 | 500 | 502 | 503 | 504)
    }
}

impl From<u16> for Status {
    fn from(code: u16) -> Self {
        Self(code)
    }
}

impl core::fmt::Display for Status {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn classes_partition_the_space() {
        assert!(Status::new(200).is_success());
        assert!(Status::new(404).is_client_error());
        assert!(Status::new(503).is_server_error());
        // A class predicate must not claim a code outside its own range.
        for code in [200u16, 299, 404, 499, 500, 599] {
            let s = Status::new(code);
            let claimed = u8::from(s.is_success())
                + u8::from(s.is_client_error())
                + u8::from(s.is_server_error());
            assert_eq!(claimed, 1, "{code} was claimed by {claimed} classes");
        }
    }

    /// The divergence this crate exists to settle: 501 and 505 are 5xx but are
    /// NOT retryable, so `is_transient` must not simply be `is_server_error`.
    #[test]
    fn permanent_5xx_refusals_are_not_transient() {
        assert!(Status::new(501).is_server_error());
        assert!(!Status::new(501).is_transient(), "501 is a permanent refusal");
        assert!(!Status::new(505).is_transient(), "505 is a permanent refusal");
        // ...while the four transient 5xx still are.
        for code in [500u16, 502, 503, 504] {
            assert!(Status::new(code).is_transient(), "{code} must be transient");
        }
    }

    #[test]
    fn the_one_transient_4xx_is_covered() {
        assert!(Status::new(408).is_transient());
        assert!(Status::new(429).is_transient());
        assert!(!Status::new(404).is_transient());
        assert!(!Status::new(403).is_transient());
    }
}
