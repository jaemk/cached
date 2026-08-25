# 0054 - Triage of stale PRs and issues

Status: Partly implemented

## Outcome

Worked through on 2026-08-24. What actually happened, which differs from the verdicts below in
two places:

- #200, #196, #203, #236: closed with a comment, as recorded.
- #64: answered and left OPEN, not closed. The record called it "answer and leave open, linked
  to 0009", which held up, but the reasoning changed. The thread has a 2025 comment working
  around the clone cost with `Box::leak`, and nobody in five years had suggested returning
  `Arc<T>`. That is now the answer given, and the `Arc` behavior is pinned by two tests in
  `tests/v3_macros.rs`, closing one of the two doc gaps this record flagged. See 0009, which
  gained a scope section: references already work where they can, and the macro path can never
  return one.
- #220 (moka): closed, but as answered-by-example rather than out of scope, which is what the
  verdict below says. Investigating it produced `examples/moka_custom_store.rs` first, and the
  issue was closed pointing at that. The finding that changed the reasoning: implementing the
  traits on a foreign cache type directly is an orphan-rule error, so a local newtype is
  mandatory for any third-party store, and the adapter is about 66 lines. That cuts both ways.
  It means moka already works through `ty` / `create`, and it means a built-in backend would
  not save a user the newtype anyway. Investigating also surfaced a gap that had nothing to do
  with moka: a custom `ty` forces the cached function to return `Result`, which now has a doc
  section and a `tests/ui` case.
- #239: left open, untouched, and deliberately deferred. The verdict below ("ask for rebase or
  close") was reached from code and GitHub state alone and does not capture why it was actually
  held. Do not act on it without asking.
- #147/#228: no action, as recorded.

The general lesson for the next triage: every verdict here was reached from code and GitHub
state, which is enough to say whether a PR still applies, but not enough to know why a
maintainer held it. Check that before acting on a close.

## Current state

Survey of open PRs and issues that predate the 3.0 rewrite, checked against current code and
GitHub state (`gh pr view` / `gh issue view`, read-only).

### PR #200 - `cache_clear` operation

Verdict: close as obsolete.

`cache_clear` already exists on every trait PR #200 would add it to, with a different shape:
`Cached::cache_clear(&mut self)` (`src/lib.rs:1423`), `ConcurrentCached::cache_clear(&self)`
(`src/lib.rs:3206`), `ConcurrentCachedAsync::async_cache_clear(&self)` (`src/lib.rs:3616`), plus
concrete store impls at `src/stores/redis.rs:1995` and `src/stores/redb.rs:1532`. (Corrects the
line numbers in the original triage brief - `src/lib.rs:1389`, `2807`, and `3217` land on
surrounding doc prose, not the fn signatures.)

PR #200's diff (`gh pr diff 200`) adds `cache_clear` to `IOCached`/`IOCachedAsync`
(`src/lib.rs`) and to `src/stores/disk.rs`. Neither target exists anymore: `IOCached` was
replaced by `ConcurrentCached`/`SerializeCached` and `src/stores/disk.rs` by `src/stores/redb.rs`
in the 3.0 rewrite. The PR is against removed API surface, not just already-superseded content;
`gh` reports `mergeable: CONFLICTING`. Its issue, #197, is closed.

### PR #196 - borrowed keys/values for `IOCached::set_cache`

Verdict: close as obsolete/superseded.

Same problem as #200: the PR's diff (`gh pr diff 196`) touches `cached_proc_macro/src/io_cached.rs`
and `src/stores/disk.rs`, both removed in 3.0 (`IOCached` and the disk store no longer exist under
those names). The functional need - setting a value without cloning it - is met on current stores
by `SerializeCached::cache_set_ref(&self, k: &K, v: &V)` (`src/lib.rs:3971`), which
`#[concurrent_cached]` routes through automatically when the concrete store implements it. See
design [0022](0022-serialize-cached-set-ref-return.md). Its issue, #195, is closed. The PR's own
body says "the interfaces are fine but the proc macros need some work" - that rework happened, in
a different shape, as part of the 3.0 macro rewrite.

### PR #203 / issue #202 - `&T` and `Option<&T>` inputs

Verdict: close as done.

Shipped in 3.0. Implemented in `cached_proc_macro/src/helpers.rs:599-624` (`strip_ref` /
`option_ref_inner`, called from the default-key path at `helpers.rs:564-579`), tested at
`tests/v3_macros.rs:84-148` (`&str` / `Option<&str>` / `&String` inputs) and `:1773-1806` (a
related fix for `Option<&mut T>` not moving the argument), and credited in `CHANGELOG.md:456-457`
("Macro ergonomics: `#[cached]` / `#[concurrent_cached]` accept reference arguments (`&T`,
`Option<&T>`) ... `[#202]`, `[#203]`"). (Corrects the brief's line numbers: `helpers.rs:551-584`
and `CHANGELOG.md:430-431` land elsewhere in those files.)

Residual gap, not a reason to keep the PR open: `strip_ref`/`option_ref_inner` strip exactly one
reference level per top-level argument type (`helpers.rs:564-579` calls each once, not
recursively). Multi-level refs (`&&T`), refs nested inside a container (`&[&T]`), and tuples of
refs passed as one macro argument are not covered by this path - unverified whether they compile
at all today (would need a `trybuild` case to confirm one way or the other), but they are
definitely not converted to an owned default key the way single-level `&T` is. This is a spec/doc
gap (see below), not a functional regression: `key`/`convert` remain an explicit escape hatch for
any input shape the default-key path does not special-case.

### PR #236 - dyn-trait example

Verdict: close as obsolete (would not compile).

The PR's diff (`gh pr diff 236`) adds `examples/dyn_trait_pass.rs` using
`#[cached(time = 100, size = 1, result = true, key = "i32", convert = ...)]`. All three of
`time =`, `size =`, and `result =` are gone from the 3.0 macro attribute surface:
`specs/macro-cached.md` CACHED-1 says TTL is `ttl_secs`/`ttl_millis`/`ttl`, "not `time =`";
CACHED-2 says `size = N` is a hard rename error to `max_size = N`
(`cached_proc_macro/src/cached.rs:457`) and that the pre-2.0 `result`/`option` attributes were
removed in favor of `cache_err`/`cache_none`. The example would fail to compile as written.
(Corrects the brief, which cited only `time` and `size`; `result = true` is a third dead
attribute in the same snippet.)

`examples/struct_method.rs` Part 3 (lines 105-230, `Processor` trait / `FastProcessor`) already
covers dispatching a cached call through a `dyn Trait` reference, keyed on a stable id -
functionally the scenario PR #236's example was written to illustrate.

### PR #239 - darling -> attrs

Verdict: ask for rebase, not 3.2 implementation work.

Opened 2025-04-22, stale (no activity since 2025-05-01) against a macro crate 3.0 rewrote.
`darling = "0.20.8"` is still the parsing dependency (`cached_proc_macro/Cargo.toml:22`), so the
compile-time argument in the PR (a `hyperfine`-measured ~25% faster clean build without it) still
applies in principle. But design [0043](0043-macro-error-precision.md) built the current
attribute-error diagnostics (one error instead of a cascade, caret at the attribute not the
function name) on darling-era parsing, with `tests/ui/` golden `.stderr` files pinning both the
error count and the span. A `darling` -> `attrs` swap has to reproduce those spans exactly or the
goldens (and the diagnostics quality they protect) regress. That is real, current-code work
against the actual dependency, not a rebase away from a removed target the way #200/#196/#236 are
- but it is a rewrite of the PR against current `cached_proc_macro` internals, not something to
merge as-is.

### Issue #64 - `Arc<T>` / returning `&V`

Verdict: answer and leave open.

The requester (issue body, `gh issue view 64`) wants to avoid cloning cached `Vec<String>`
values on every read. The documented mitigation - return `Arc<T>` from the cached function so
reads clone a pointer, not the value - lives at `src/lib.rs:813-816` ("When the return value is
expensive to clone, return `Arc<T>` from the cached function: the cache stores the `Arc` and
every hit clones only the pointer, not `T`."). (Corrects the brief's `src/lib.rs:779-782`, which
is earlier prose in the same doc comment, not this sentence.)

The deeper ask - `cache_get` returning `&V` instead of a clone - is design
[0009](0009-cached-get-shared-receiver.md), status "Needs research", deferred as too invasive
(would need interior mutability for LRU recency and hit/miss counters, and risks a
RefCell-style borrow panic on the shared-borrow path). Not a fit for 3.2 on its own.

Two doc gaps surfaced by this issue, worth closing in 3.2 independent of 0009:
- No clause in `specs/macro-cached.md` documents the `Arc<T>` pattern (confirmed: no `Arc`
  mention anywhere in that file).
- No test asserts the pattern actually avoids the clone. `tests/v3_macros.rs` has no `Arc`
  reference at all (confirmed via search); a `#[cached] fn -> Arc<T>` test asserting
  `Arc::ptr_eq(&first_call, &second_call)` on a cache hit would pin the claim.

### Issue #220 - moka

Verdict: out of scope, leave open or close as won't-fix (maintainer's call; no code action).

The requester's own comment on the issue: "I forked this project and implemented moka there:
https://crates.io/crates/kash. Waiting for this feature." The ask has a working fork; nothing in
this codebase currently depends on it.

### Issues #147 / #228 - stale-while-revalidate

Verdict: answered, and the one open follow-on has since shipped.

Both are answered by `examples/stale_while_revalidate.rs`, added in two PRs whose commit bodies
name the issues directly: `c966522` ("Add a stale-while-revalidate example", #310) ends "Answers
#147"; `89a1024` ("Add single-flight revalidation to the stale-while-revalidate example", #311)
begins "Follows #228, which asks for per-key write synchronization that returns a stale value
instead of blocking". Neither issue nor the CHANGELOG cross-references the other by number, but
the content match is direct: #147 asks to "update cached value asynchronously, outside the thread
that returns the return value"; #228 asks for a "stale-while-revalidate feature" - both are what
the example demonstrates.

Note the distinction between two things that are easy to conflate. `b4c8d8c` ("release the
stale-while-revalidate refresh claim from `Drop`", #313) fixed the claim leak inside
`examples/stale_while_revalidate.rs`, so a panicking or aborted refresh now releases its claim
rather than pinning the key to a stale value forever. That is example code. It did not add any
library API. 0053 proposes promoting the guard into the crate, and it has not shipped.

So the state of these two issues is: answered by the example, with no action needed now. If 0053
ships, update them to point at the API rather than the example, rather than closing them on the
strength of #313 alone.

## Desired work

Doc/spec gaps this triage surfaced, worth closing in 3.2 (no code behavior change):

- Add a reference-input clause to `specs/macro-cached.md` (a new `CACHED-N`) documenting that
  `#[cached]`/`#[concurrent_cached]` accept `&T`/`Option<&T>` on the default-key path
  (`helpers.rs:599-624`), and stating the residual gap explicitly: only one reference level is
  stripped per top-level argument, so `&&T`, `&[&T]`, and tuples containing refs are not
  special-cased and need an explicit `key`/`convert` (or a `trybuild` case first, to confirm
  whether they fail to compile or silently key on the wrong type).
- Add an `Arc<T>` clause to `specs/macro-cached.md` pointing at the existing `src/lib.rs:813-816`
  guidance, and add a `#[cached] fn -> Arc<T>` test in `tests/v3_macros.rs` asserting
  `Arc::ptr_eq` on a cache hit.

PR/issue actions for a maintainer to take (this record does not take them - no GitHub mutation
was made in producing it):

- #200: close as obsolete (targets removed `IOCached`/`src/stores/disk.rs`; `cache_clear` already
  shipped under `Cached`/`ConcurrentCached`/`ConcurrentCachedAsync`).
- #196: close as obsolete/superseded (targets the same removed files; need met by
  `SerializeCached::cache_set_ref`, design 0022).
- #203 (and #202, already closed): close as done (shipped in 3.0; note the multi-level-ref gap in
  the new spec clause rather than reopening).
- #236: close as obsolete (uses three removed/renamed macro attributes; would not compile;
  `examples/struct_method.rs` Part 3 already covers the underlying dyn-Trait scenario).
- #239: ask for rebase (the `darling` dependency and the compile-time motivation both still stand,
  but the PR predates design 0043's darling-era diagnostics and `tests/ui/` goldens and needs to
  be redone against current `cached_proc_macro`, not merged as-is).
- #64: answer with the `Arc<T>` pattern (point at `src/lib.rs:813-816`) and leave open, linked to
  design 0009 for the deeper `&V`-returning ask.
- #220: no action; requester already has a working fork (`kash`).
- #147 / #228: no action; answered by `examples/stale_while_revalidate.rs`. #313 fixed that
  example's own claim leak but added no library API, so revisit only if 0053 ships.

## Notes

- None of the PR `mergeable` states reported by `gh` (`CONFLICTING` for #200, `UNKNOWN` for #196,
  #203, #236, #239) were used as the basis for any verdict above; every verdict rests on reading
  the actual diff (`gh pr diff <n>`) against current source, not GitHub's merge-conflict computation.
- This record makes no GitHub API writes. All `gh` calls were read-only (`gh pr view`, `gh pr
  diff`, `gh issue view`).

## Verification

- Verified directly: all `src/lib.rs` / store / macro-source line numbers cited above (re-read
  after the original triage brief's numbers were found to be off by one section in three places -
  see the corrections noted inline for #200, #203, and #64); all PR/issue states and bodies via
  `gh pr view --json`, `gh pr diff`, `gh issue view --json`; the `IOCached` trait and
  `src/stores/disk.rs` no longer existing (`grep`/`ls` against current `src/`); the `darling`
  dependency still present at `cached_proc_macro/Cargo.toml:22`; the `Arc<T>` doc gap and the
  `Arc` test gap (both confirmed absent by search, not merely assumed).
- Could not verify: whether `&&T`, `&[&T]`, or tuple-of-refs macro arguments currently compile at
  all under the default-key path (no existing test either way; would need a new `trybuild` case in
  `tests/ui/` to settle, left as part of the desired-work item above rather than asserted here).
- Could not verify: GitHub's stated `mergeable` field reliability (it is a best-effort background
  computation); not relied upon for any verdict, per the note above.
