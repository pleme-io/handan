//! # handan (判断) — judgment of an HTTP response
//!
//! *handan* is Japanese for **judgment**: the crate looks at what a server
//! answered and says which of a closed set of things happened, and how long to
//! wait. Zero dependencies, no async runtime, no HTTP client — so the same
//! judgment serves `reqwest`, `ureq`, `hyper`, or a status scraped out of a
//! subprocess's output.
//!
//! ```
//! use handan::{Verdict, judge};
//!
//! // GitHub's secondary rate limit: a 403 that is really a throttle.
//! let j = judge(403, Some("60"));
//! assert_eq!(j.verdict(), Verdict::Throttled);
//! assert!(j.is_transient());
//!
//! // A genuine authorization failure, same status code.
//! assert_eq!(judge(403, None).verdict(), Verdict::Unauthorized);
//! ```
//!
//! ## Why this is a crate and not a method
//!
//! Six pleme-io crates independently derived this same shape, none of them
//! aware of the others — no shared ancestor, no cross-reference, no shared
//! dependency:
//!
//! | site | its derivation |
//! |---|---|
//! | `todoku` | `parse_retry_after`; `retry_statuses: {429, 500, 502, 503, 504}` |
//! | `acervo-net` | `StatusCode` newtype; `RateLimited { retry_after_secs }` |
//! | `sui-spec` | `HttpError::Throttled { retry_after }` + `UnexpectedStatus` |
//! | `kenshi` | `RateLimited { retry_after_secs }` |
//! | `forge` | `is_transient` |
//! | `fleet` | string-matched `"HTTP error 429"` out of another tool's English prose |
//!
//! Duplication says a shape is *convenient*. Six independent derivations —
//! two of them landing on the identical *constant* — says the shape is **forced
//! by the problem**, which is the stronger signal and the reason this is
//! extracted rather than left alone. It is owned by none of the six.
//!
//! The `fleet` row is the one worth dwelling on: it recovered a typed fact by
//! grepping prose, so it broke whenever the upstream reworded an error, and it
//! could not distinguish a throttle from a 404 when both appeared in the same
//! stderr. That is what a missing type costs.
//!
//! ## What extraction settled
//!
//! Two of the six **disagreed**, and merging them forced a decision rather than
//! preserving both:
//!
//! - `todoku` retried `{429, 500, 502, 503, 504}`; `acervo-net` retried every
//!   `5xx`. The explicit set wins — `501 Not Implemented` is a *permanent*
//!   refusal, so retrying it is waste that also consumes a retry budget.
//!   See [`Status::is_transient`].
//! - Nobody parsed the **HTTP-date** form of `Retry-After`, so a `503` or `429`
//!   carrying it was read fleet-wide as *no advice at all*. handan parses both
//!   forms with no dependency. See [`parse_retry_after`].
//!
//! ## It classifies the status, never the situation
//!
//! The judgment is a reading of what the *protocol* said. Where a caller knows
//! more about the *situation*, it should override rather than defer — and one
//! case matters enough to name here, because it is a trap and it looks like a
//! bug in this crate:
//!
//! **A `404` does not prove absence on a private resource.** GitHub answers
//! `404` rather than `401` for a private repo fetched without credentials, so
//! that a `401` does not disclose the repo exists. [`Verdict::Absent`] is
//! therefore the safe *generic* reading of a `404`, not a proof, and a consumer
//! that knows the resource is private is right to read the same status as
//! [`Verdict::Unauthorized`]. `fleet` does exactly that for private flake
//! inputs and it is not deviating in error.
//!
//! ## What it deliberately does not do
//!
//! It renders no error type and owns no retry loop. Each consumer's error enum
//! carries domain-specific arms (`acervo` has `Shape`, `sui-spec` has
//! `BadUrl`/`UnsupportedScheme`) and forcing those into one type would produce
//! an abstraction that fits none of them. Those stay where they are and *derive*
//! their classification from here — same shape extracted, different shapes left
//! alone.

#![forbid(unsafe_code)]

mod advice;
mod status;
mod verdict;

pub use advice::{RetryAdvice, parse_retry_after};
pub use status::Status;
pub use verdict::{Judgment, Verdict, judge, judge_signalled};
