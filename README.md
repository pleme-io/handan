# handan (判断)

**Judgment of an HTTP response.** Zero dependencies. Says which of a closed set
of things a server answered, and how long to wait.

```rust
use handan::{judge, Verdict};

// GitHub's secondary rate limit: a 403 that is really a throttle.
let j = judge(403, Some("60"));
assert_eq!(j.verdict(), Verdict::Throttled);
assert!(j.is_transient());

// A genuine authorization failure — the same status code.
assert_eq!(judge(403, None).verdict(), Verdict::Unauthorized);
```

No async runtime, no HTTP client, no `serde`. It takes a `u16` and a `&str`, so
one judgment serves `reqwest`, `ureq`, `hyper`, or a status recovered from
another tool's output.

## Why it exists

Six crates in the pleme-io fleet independently derived this same shape, none
aware of the others — verified: no shared ancestor, no cross-reference, no
shared dependency.

| site | its derivation |
|---|---|
| `todoku` | `parse_retry_after`; `retry_statuses: {429, 500, 502, 503, 504}` |
| `acervo-net` | `StatusCode` newtype; `RateLimited { retry_after_secs }` |
| `sui-spec` | `HttpError::Throttled { retry_after }` + `UnexpectedStatus` |
| `kenshi` | `RateLimited { retry_after_secs }` |
| `forge` | `is_transient` |
| `fleet` | string-matched `"HTTP error 429"` out of another tool's English prose |

Duplication says a shape is *convenient*. Six independent derivations — two of
them landing on the identical *constant* — says the shape is **forced by the
problem**. That is the stronger signal, and the reason this is extracted rather
than left alone.

## What extraction settled

Two of the six **disagreed**, and merging them forced a decision instead of
preserving both:

- **`501` is not retryable.** `todoku` retried `{429, 500, 502, 503, 504}`;
  `acervo-net` retried every `5xx`. The explicit set wins — `501 Not Implemented`
  and `505` are *permanent* refusals, so retrying them is waste that also
  consumes a retry budget a transient failure needed. `408 Request Timeout` is
  added on top of both: it is the one `4xx` whose whole meaning is "try again",
  and neither original covered it.
- **`Retry-After` has two forms and nobody parsed the second.** A `503` or `429`
  carrying the HTTP-date form read fleet-wide as *no advice at all*, so every
  consumer discarded the one number the server volunteered and guessed instead.
  handan parses both, with no dependency: IMF-fixdate is a fixed ASCII grammar,
  so integer arithmetic suffices.

## Two design choices worth knowing

**The header participates in the verdict.** A `403` means opposite things with
and without `Retry-After` — GitHub sends `403` + `Retry-After` for a secondary
rate limit (wait it out) and bare `403` for a real denial (fix credentials). A
status-only classifier must round one into the other, and both roundings send
the operator the wrong way. Callers holding the *signal* but no parseable value
use [`judge_signalled`] rather than fabricating a header.

**It classifies the status, never the situation.** A `404` does not prove
absence on a *private* resource: GitHub answers `404` instead of `401` there on
purpose, so a `401` cannot disclose the resource exists. `Verdict::Absent` is
the safe generic reading, and a consumer that knows the resource is private is
right to override it.

## Install

```toml
[dependencies]
handan = "0.1"
```

MIT.
