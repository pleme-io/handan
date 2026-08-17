//! The judgment itself: which of a closed set of things the response said.

use crate::advice::{RetryAdvice, parse_retry_after};
use crate::status::Status;
use core::time::Duration;

/// What a response *meant*, as a closed set.
///
/// This is the [`kotae`] rule applied to an HTTP status: an answer says **which**
/// of N things happened, and no two arms render the same bytes. The point is not
/// tidiness — it is that each arm has a *different remedy*, and a caller holding
/// a string cannot branch on remedy:
///
/// | verdict | remedy |
/// |---|---|
/// | `Success` | use the body |
/// | `Unchanged` | keep the cached copy |
/// | `Absent` | stop; the thing is not there |
/// | `Unauthorized` | stop; fix credentials — retrying cannot help |
/// | `Throttled` | wait (see the advice), **or fetch identical bytes via another egress** |
/// | `ServerError` | wait; the same request may succeed |
/// | `Other` | escalate with the code preserved |
///
/// `Throttled` is deliberately separate from `ServerError` even though both say
/// "wait", because only `Throttled` admits the second remedy: the upstream *has*
/// the content and is refusing to serve it to *us*, so another network egress
/// can fetch the identical bytes — verifiable against a pinned hash.
///
/// [`kotae`]: https://github.com/pleme-io/kotae
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Verdict {
    /// `2xx` — the request succeeded.
    Success,
    /// `304` — the caller's cached copy is still current.
    Unchanged,
    /// `404` / `410` — the resource is not there.
    ///
    /// ── ★ A `404` DOES NOT PROVE ABSENCE ON A PRIVATE RESOURCE ────────────
    ///
    /// GitHub — and every host that follows its lead — answers `404` rather than
    /// `401` for a private resource fetched **without credentials**, on purpose:
    /// a `401` would disclose that the repository exists. So for a resource the
    /// caller believes is private, `Absent` and [`Verdict::Unauthorized`] are
    /// **not distinguishable from the status**, and this arm is the safe generic
    /// reading, not a proof.
    ///
    /// A caller that knows more should override rather than defer. `fleet` is
    /// the worked example: it classifies private flake inputs, where a `404` on
    /// the codeload `/archive/` shape reliably means the token never arrived, so
    /// it maps that case to `Unauthorized` against this verdict — correctly.
    /// That is a legitimate domain override, and it is the reason this crate
    /// classifies the *status* and never pretends to classify the *situation*.
    Absent,
    /// `401` / `403` with no retry advice — the caller lacks authority.
    Unauthorized,
    /// `429`, or any status carrying retry advice — refused, but not forever.
    Throttled,
    /// `5xx` — the upstream broke.
    ServerError,
    /// Anything else. The code is preserved on the [`Judgment`].
    Other,
}

impl core::fmt::Display for Verdict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::Success => "success",
            Self::Unchanged => "unchanged",
            Self::Absent => "absent",
            Self::Unauthorized => "unauthorized",
            Self::Throttled => "throttled",
            Self::ServerError => "server-error",
            Self::Other => "other",
        };
        f.write_str(s)
    }
}

/// A status, its verdict, and the server's retry advice — as one value.
///
/// ── ★ THE FIELDS ARE PRIVATE ON PURPOSE ───────────────────────────────────
///
/// A `Judgment` can only be produced by [`judge`], so a value claiming
/// `Verdict::Success` while carrying status `429` has no constructor. That is
/// the illegal state this type exists to remove: every one of the six fleet
/// call sites that this crate replaces held the status and its interpretation
/// as *two independent values*, free to disagree — and in `fleet`'s case the
/// interpretation was recovered by grepping English prose, so they routinely
/// did.
///
/// Tier: **parse-time-rejected**, not truly-unrepresentable — `judge` is a
/// total function over `u16`, so the guarantee is that no inconsistent pair can
/// be *constructed*, not that the type system proves consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Judgment {
    status: Status,
    verdict: Verdict,
    advice: Option<RetryAdvice>,
}

impl Judgment {
    /// The raw status.
    #[must_use]
    pub const fn status(&self) -> Status {
        self.status
    }

    /// Which of the closed set happened.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// The server's own advice, if it gave any and it was parseable.
    #[must_use]
    pub const fn advice(&self) -> Option<RetryAdvice> {
        self.advice
    }

    /// Whether sending the identical request later could plausibly succeed.
    ///
    /// Note the `Throttled` arm is load-bearing and not redundant with
    /// [`Status::is_transient`]: a `403` carrying `Retry-After` is judged
    /// `Throttled` and *is* retryable, while `403` alone is not in the
    /// transient status set. Deriving this from the status alone would
    /// misclassify GitHub's secondary rate limit as a permanent refusal.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self.verdict, Verdict::Throttled) || self.status.is_transient()
    }

    /// How long to wait before retrying, if the server said.
    ///
    /// `None` means *no advice was given* — the caller falls back to its own
    /// backoff. It does **not** mean "wait zero".
    #[must_use]
    pub const fn delay_from(&self, now_unix_secs: u64) -> Option<Duration> {
        match self.advice {
            Some(a) => Some(a.delay_from(now_unix_secs)),
            None => None,
        }
    }
}

/// Judge a response: status code plus the raw `Retry-After` header, if present.
///
/// Takes `u16` and `&str` rather than any HTTP library's types, so the same
/// judgment serves `reqwest` (async), `ureq` (sync), a raw `hyper` response, or
/// a status recovered from a subprocess — which is the whole reason this is a
/// separate zero-dependency crate rather than a method on one client.
///
/// ── ★ WHY THE HEADER IS AN INPUT TO CLASSIFICATION ────────────────────────
///
/// A `403` means two entirely different things depending on this header, and
/// the remedies are opposites. GitHub returns `403` **with** `Retry-After` for
/// a secondary rate limit — a temporary refusal that a caller should wait out —
/// and `403` **without** it for a genuine authorization failure, where retrying
/// is waste and the caller must fix credentials. A status-only classifier
/// cannot tell them apart, so it must round one into the other; both roundings
/// are harmful. Hence the header participates in the verdict rather than merely
/// decorating it.
#[must_use]
pub fn judge(status: u16, retry_after: Option<&str>) -> Judgment {
    let advice = retry_after.and_then(parse_retry_after);
    Judgment {
        status: Status::new(status),
        verdict: classify(status, advice.is_some()),
        advice,
    }
}

/// Judge from a status plus an out-of-band signal that the refusal is temporary.
///
/// Use this when you have the *signal* but no parseable value: a `Retry-After`
/// you could not read, a vendor header, or a message recovered from another
/// tool's output. `fleet` is the motivating caller — it classifies a failed
/// `nix` invocation from stderr, so it never holds a response header at all,
/// and nix prints the secondary-rate-limit text from the response *body*.
///
/// The distinction matters because the header's only contribution to the
/// *verdict* is boolean — whether the server volunteered that it would serve
/// this later. The value contributes to the *delay*, which a caller with no
/// header simply does not get. Exposing that boolean directly is honest;
/// fabricating a header value to pass to [`judge`] would put an invented
/// number in [`Judgment::advice`] where a reader would reasonably trust it.
#[must_use]
pub fn judge_signalled(status: u16, temporary_refusal_signalled: bool) -> Judgment {
    Judgment {
        status: Status::new(status),
        verdict: classify(status, temporary_refusal_signalled),
        advice: None,
    }
}

/// The one classification rule, shared by both entry points.
fn classify(code: u16, temporary_refusal_signalled: bool) -> Verdict {
    let status = Status::new(code);

    if status.is_success() {
        Verdict::Success
    } else if code == 304 {
        Verdict::Unchanged
    } else if code == 429 {
        Verdict::Throttled
    } else if matches!(code, 401 | 403) {
        // The signal decides — see the note above.
        if temporary_refusal_signalled {
            Verdict::Throttled
        } else {
            Verdict::Unauthorized
        }
    } else if matches!(code, 404 | 410) {
        Verdict::Absent
    } else if status.is_server_error() {
        Verdict::ServerError
    } else {
        Verdict::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{Verdict, judge};
    use crate::advice::RetryAdvice;
    use core::time::Duration;

    #[test]
    fn the_ordinary_categories() {
        assert_eq!(judge(200, None).verdict(), Verdict::Success);
        assert_eq!(judge(204, None).verdict(), Verdict::Success);
        assert_eq!(judge(304, None).verdict(), Verdict::Unchanged);
        assert_eq!(judge(404, None).verdict(), Verdict::Absent);
        assert_eq!(judge(410, None).verdict(), Verdict::Absent);
        assert_eq!(judge(401, None).verdict(), Verdict::Unauthorized);
        assert_eq!(judge(429, None).verdict(), Verdict::Throttled);
        assert_eq!(judge(500, None).verdict(), Verdict::ServerError);
        assert_eq!(judge(418, None).verdict(), Verdict::Other);
    }

    /// The distinction a status-only classifier structurally cannot make.
    #[test]
    fn a_403_with_retry_advice_is_a_throttle_not_an_auth_failure() {
        let secondary_limit = judge(403, Some("60"));
        assert_eq!(secondary_limit.verdict(), Verdict::Throttled);
        assert!(secondary_limit.is_transient(), "GitHub's secondary rate limit is retryable");

        let real_denial = judge(403, None);
        assert_eq!(real_denial.verdict(), Verdict::Unauthorized);
        assert!(!real_denial.is_transient(), "a genuine 403 must not be retried");
    }

    /// The property the whole crate exists for: distinct causes must not render
    /// the same bytes. Compared at a CONSTANT status where that is meaningful,
    /// and across statuses otherwise — varying the status would let the status
    /// itself carry the difference and the test would pass while the verdicts
    /// were identical.
    #[test]
    fn no_two_causes_render_the_same_verdict_bytes() {
        // Same status, different advice ⇒ different verdict.
        let a = judge(403, Some("60")).verdict().to_string();
        let b = judge(403, None).verdict().to_string();
        assert_ne!(a, b, "403±Retry-After must not render alike");

        // Every arm renders distinctly.
        let arms = [
            Verdict::Success,
            Verdict::Unchanged,
            Verdict::Absent,
            Verdict::Unauthorized,
            Verdict::Throttled,
            Verdict::ServerError,
            Verdict::Other,
        ];
        let mut seen = std::collections::HashSet::new();
        for arm in arms {
            assert!(seen.insert(arm.to_string()), "{arm:?} renders as an existing arm");
        }
        assert_eq!(seen.len(), 7, "an arm was added without a distinct rendering");
    }

    #[test]
    fn advice_is_carried_through_in_both_forms() {
        assert_eq!(
            judge(429, Some("30")).advice(),
            Some(RetryAdvice::After(Duration::from_secs(30)))
        );
        assert_eq!(
            judge(503, Some("Sun, 06 Nov 1994 08:49:37 GMT")).advice(),
            Some(RetryAdvice::At {
                unix_secs: 784_111_777
            })
        );
        // Unparseable advice is *absent* advice, never a fabricated zero.
        assert_eq!(judge(429, Some("soon")).advice(), None);
        assert_eq!(judge(429, Some("soon")).delay_from(0), None);
    }

    /// A `429` with an unreadable header must still be judged a throttle — the
    /// classification must not depend on the advice parsing.
    #[test]
    fn an_unreadable_header_does_not_downgrade_the_verdict() {
        let j = judge(429, Some("whenever"));
        assert_eq!(j.verdict(), Verdict::Throttled);
        assert!(j.is_transient());
        assert_eq!(j.advice(), None);
    }

    #[test]
    fn transience_agrees_with_the_status_set_except_where_advice_overrides() {
        assert!(judge(503, None).is_transient());
        assert!(judge(408, None).is_transient());
        assert!(!judge(501, None).is_transient(), "501 is permanent");
        assert!(!judge(404, None).is_transient());
        assert!(!judge(200, None).is_transient());
    }

    /// `judge_signalled` must reach the same verdict as `judge` for every
    /// status, given the same boolean — they share one rule, and a second
    /// entry point that drifted from the first would be worse than none.
    #[test]
    fn both_entry_points_agree_on_every_status() {
        for code in 100u16..600 {
            assert_eq!(
                super::judge_signalled(code, false).verdict(),
                judge(code, None).verdict(),
                "{code} disagreed with no signal"
            );
            assert_eq!(
                super::judge_signalled(code, true).verdict(),
                judge(code, Some("30")).verdict(),
                "{code} disagreed with a signal"
            );
        }
    }

    /// The out-of-band form must not invent advice it never received.
    #[test]
    fn a_signalled_judgment_carries_no_fabricated_advice() {
        let j = super::judge_signalled(403, true);
        assert_eq!(j.verdict(), Verdict::Throttled);
        assert!(j.is_transient());
        assert_eq!(j.advice(), None, "no header was read, so no advice may be reported");
        assert_eq!(j.delay_from(0), None);
    }

    #[test]
    fn a_judgment_cannot_be_built_inconsistently() {
        // There is no public constructor other than `judge`, so this is the only
        // way a Judgment exists — the status and verdict cannot disagree.
        let j = judge(429, None);
        assert_eq!(j.status().code(), 429);
        assert_eq!(j.verdict(), Verdict::Throttled);
    }
}
