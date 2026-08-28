# 0053 - A first-class refresh-claim guard

Status: Implemented

## Outcome

Shipped as `src/claim.rs`: `ClaimRegistry<K>` (`K: Eq + Hash + Clone`), `Claim<K>`, and the
`#[must_use]` `claim`/`is_claimed`/`len`/`is_empty` surface, matching the proposed surface in
"Desired work" exactly, including the `Clone`-over-`Arc<K>` tradeoff and `parking_lot::Mutex`.

Two points the record left open:

- **Crate-root naming.** The record said "consider exporting only through `cached::claim::` plus
  the prelude" (Pitfalls). That is what shipped: `pub mod claim;` at `src/lib.rs:915`, with
  `Claim`/`ClaimRegistry` re-exported through `cached::prelude` (`src/lib.rs:1203`) and nowhere at
  the crate root, for the same `KeyedCache` nearest-match-suggestion reason recorded at
  `src/lib.rs:1143-1145`.
- **Capacity: shrink on empty vs. document the high-water mark.** The record posed this as a
  decision to make (Pitfalls, "Capacity is a high-water mark"). It shipped documented and left
  alone, not shrunk: the module doc's "Capacity" section (`src/claim.rs:87-93`) states the set
  keeps its peak allocation for the life of the registry and recommends a registry per key space
  where that bound matters, rather than calling `shrink_to_fit` on drain to empty.

`examples/stale_while_revalidate.rs` and `examples/refresh_before_expiry.rs` are being rewritten
onto the shipped type in a separate change; not evaluated here.

## Current state

Both background-refresh recipes need to collapse concurrent refreshes of one key onto a single
caller, and both hand-roll the same thing: a `Mutex<Option<HashSet<String>>>`, a `claim_refresh`
that inserts, and a `release_refresh` that removes.

- `examples/stale_while_revalidate.rs:181-223` (`REFRESHING` static at 181, `RefreshClaim` and its
  `Drop` at 183-198, `claim_refresh` at 200-214, `release_refresh` at 216-223).
- `examples/refresh_before_expiry.rs:466-508` (the same four items at 466, 468-483, 485-499,
  501-508).

Those two blocks are the same 43 lines, duplicated. The only difference between the files is the
`use` line above them (`stale_while_revalidate.rs:40-42` versus
`refresh_before_expiry.rs:85-87`).

The claim is consumed by borrowing the key out of the guard and passing it straight to the prime
companion, so the guard stays alive for exactly the refresh:

```rust
if let Some(claim) = claim_refresh(id) {
    tokio::spawn(async move {
        async_lookup_prime_cache(claim.key()).await;
    });
}
```

(`examples/stale_while_revalidate.rs:234-242`, `examples/refresh_before_expiry.rs:518-529`, and
again at `stale_while_revalidate.rs:367-371` for the `sync_writes = "by_key"` variant.)

### The bug this shape fixes

The first version released the claim by hand at the end of the spawned task:
`release_refresh(&owned)` as the last statement, added in c966522 (#310). That statement is
skipped when the refresh body panics or the task is aborted, so the key stays claimed for the life
of the process. Because the peek deliberately never removes an expired entry, the entry is then
served stale forever and no caller can ever recompute it: strictly worse than not deduplicating at
all. b4c8d8c (#313) replaced it with the RAII guard in `stale_while_revalidate.rs`, and
`refresh_before_expiry.rs` (added in d832c0a, #312) carries the fixed shape from the start. The
reasoning is recorded inline at `examples/stale_while_revalidate.rs:173-178` and
`examples/refresh_before_expiry.rs:458-463`.

Both examples now assert all of it, and `make examples` runs them in CI (`Makefile:13-26`,
`Makefile:41-47`, `Makefile:112`):

- a panicking refresh releases its claim and a retry succeeds
  (`stale_while_revalidate.rs:489-516`, `refresh_before_expiry.rs:908-935`),
- an aborted (cancelled) task releases its claim (`stale_while_revalidate.rs:518-541`,
  `refresh_before_expiry.rs:937-962`),
- a claim dropped over a poisoned mutex releases instead of panicking a second time
  (`stale_while_revalidate.rs:640-673`, `refresh_before_expiry.rs:1009-1043`).

That last one exists because `release_refresh` runs from `Drop`, including during an unwind, where
a second panic aborts the process. Both files recover with
`.lock().unwrap_or_else(PoisonError::into_inner)` (`stale_while_revalidate.rs:216-223`,
`refresh_before_expiry.rs:501-508`) while `claim_refresh` keeps `.expect`ing a healthy lock,
because it never runs from a `Drop` context.

### Why this is library-shaped

The crate's current answer is "write it yourself": `ConcurrentCached::cache_get_or_set_with`
documents "if you need the body to run at most once per key, serialize the call yourself (for
example behind your own per-key lock)" (`src/lib.rs:3237-3241`). The evidence that the DIY answer
is not good enough is that the same bug was written twice, in the crate's own examples, by the
author of the recipe, and its failure mode is silent and permanent.

The standing objection to shipping background refresh as a feature does not apply here. It is
recorded in the examples themselves ("Spawning is left to the caller on purpose: `cached` has no
runtime dependency, so it cannot pick tokio or smol on your behalf",
`examples/stale_while_revalidate.rs:19-21` and `examples/refresh_before_expiry.rs:49-51`) and in
0047 ("Refresh policy stays with the caller", 0047 "Rationale"), and it is an objection to
**spawning**. A claim registry spawns nothing, awaits nothing, and needs no executor: it is a set
plus a `Drop`. Spawning stays with the caller, exactly as it is today. That distinction is the
reason this is proposable at all, and it should lead any changelog or issue reply about it.

## Desired work

A standalone type, not a trait. There is one possible implementation, nothing for a store to
customize, and the registry is deliberately independent of any store so it works with `#[cached]`,
`#[concurrent_cached]`, and hand-rolled caches alike. 0045 already sets the rule that a trait is
for a capability implementors provide differently; this is not that.

Proposed surface:

```rust
// src/claim.rs -> pub mod claim, re-exported at the crate root and in `cached::prelude`
pub struct ClaimRegistry<K> { /* Arc<Mutex<HashSet<K>>>, Clone shares */ }

impl<K: Eq + Hash + Clone> ClaimRegistry<K> {
    pub fn new() -> Self;
    #[must_use = "the claim releases the key when dropped; bind it for the whole refresh"]
    pub fn claim(&self, key: K) -> Option<Claim<K>>;
    pub fn is_claimed<Q>(&self, key: &Q) -> bool where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

pub struct Claim<K: Eq + Hash> { /* registry handle + key */ }
impl<K: Eq + Hash> Claim<K> { pub fn key(&self) -> &K; }
impl<K: Eq + Hash> Drop for Claim<K> { /* remove the key */ }
```

Points that are decisions, not details:

- **Generic over the key only.** `K: Eq + Hash` for the set, plus `Clone` because the set owns one
  copy and the claim owns the copy that `key()` hands to the prime companion. The alternative that
  drops `Clone` is `HashSet<Arc<K>>` with the claim holding the same `Arc<K>` (`Arc<K>: Borrow<K>`
  keeps lookups by `&K` working), trading one allocation per claim for one bound. Take the `Clone`
  route: every key type these recipes produce is a macro-generated `String` or tuple that is
  already `Clone`. There is no `V` and no store type anywhere in the signature.
- **Owned handle, not a borrow.** `Claim<K>` holds a cheap clone of the registry handle rather
  than a `&'a ClaimRegistry<K>`. The use sites move the claim into a spawned task
  (`stale_while_revalidate.rs:234-242`), which needs `'static`; a borrowing `Claim<'a, K>` only
  reaches `'static` when the registry is a `static`, which rules out holding a registry in a
  struct field. The cost is that `new()` cannot be `const`, so a `static` registry needs
  `LazyLock`. That is not a regression: `HashSet::new()` is not const-constructible either, which
  is exactly why both examples declare `Mutex<Option<HashSet<String>>>` and lazily
  `get_or_insert_with` (`stale_while_revalidate.rs:181`, `204-207`).
- **`key()` is load-bearing, not a convenience.** Because the refresh borrows its key out of the
  guard, the guard cannot be dropped before the refresh finishes without a borrow error. The
  correct usage is the one the compiler already enforces.
- **No async variant, one type for both.** The guard is a `Drop` impl, and `Drop` runs the same
  way for a thread that returns, a thread that unwinds, a task that completes, and a task whose
  future is dropped mid-poll (cancellation). The only async-specific requirement is that
  `Claim<K>` be `Send` for `tokio::spawn`, which holds when `K: Send + 'static`. Nothing here
  touches `async`/`async_core`.
- **No feature gate.** `parking_lot` is an unconditional dependency (`Cargo.toml:94-95`) and the
  rest is `std`. No `time_stores`, no `async`, no new dependency.
- **Use `parking_lot::Mutex`, which does not poison.** That deletes the whole poison problem from
  user code, including the `PoisonError::into_inner` recovery both examples needed
  (`stale_while_revalidate.rs:216-223`, `refresh_before_expiry.rs:501-508`) and the two example
  sections that exist only to prove it (`stale_while_revalidate.rs:640-673`,
  `refresh_before_expiry.rs:1009-1043`). If `std::sync::Mutex` is used instead for any reason, the
  release path MUST recover with `unwrap_or_else(PoisonError::into_inner)`, because it runs from
  `Drop` during an unwind where a second panic aborts the process, and `claim` should recover too
  rather than hand a library user a panic: the set's invariant is a set of keys, which a panic
  elsewhere cannot corrupt.
- **Do not implement `Clone` for `Claim`.** Two live claims on one key is the bug the type exists
  to prevent. `ClaimRegistry` is `Clone` (shared handle); `Claim` is not.

## Pitfalls

- **Three release paths, not one.** Completion, unwind, and cancellation (a dropped future). A
  test that only proves the first two passes against a `Drop` that is never reached by an aborted
  task. Both examples already drive all three (`stale_while_revalidate.rs:488-541`,
  `refresh_before_expiry.rs:908-962`) and the abort case needs the deliberate
  "let the task start, then abort" shape at `stale_while_revalidate.rs:521-531`: aborting a task
  the runtime never polled proves nothing about a claim held mid-execution.
- **The registry is a leak if a claim is ever leaked.** `mem::forget`, `Box::leak`, or a claim
  parked in a long-lived collection wedges that key permanently, which is precisely the failure
  #313 fixed, reintroduced from the other end. Expose `len`/`is_empty` so tests can assert the
  registry drains, document that a `Claim` must not be leaked, and say plainly that claiming on a
  path where no refresh follows is a bug (claim after the decision, never before it).
- **Capacity is a high-water mark.** The `HashSet` keeps its peak allocation for the process
  lifetime even when it drains to empty. Over a large key space with bursty refreshes that is a
  real, if modest, retained allocation. Decide whether `claim` shrinks on empty or whether it is
  documented and left alone.
- **A claim is not a lock and must never become one.** The registry mutex is held only across the
  set insert or remove, never across the recompute or the store write; holding it across user code
  would serialize every key, not one. The recipe exists because `{fn}_prime_cache` runs the body
  BEFORE taking the cache write lock, so a refresh never blocks the readers being served the stale
  value (`examples/stale_while_revalidate.rs:12-14`). A guard that reintroduces that blocking
  defeats the recipe.
- **It does not deduplicate the cold path, and must not claim to.** With nothing cached, the
  claim's `None` branch would leave the losing callers with no value to serve. `sync_writes =
  "by_key"` is what dedupes the cold path, and it covers the store write rather than the body
  (`examples/stale_while_revalidate.rs:167-171`, section 5 at 322-377, and `KeyedCache` at
  `src/lib.rs:1147-1176`). The two compose; they do not substitute for each other.
- **Crate-root naming.** `Claim` and `ClaimRegistry` are generic English words landing next to the
  store types at the crate root, and rustc offers root names as nearest-match suggestions for
  unrelated mistyped imports, which is the exact problem recorded for `KeyedCache` at
  `src/lib.rs:1143-1145`. Consider exporting only through `cached::claim::` plus the prelude.

## Verification

- `ulimit -v 8000000; cargo test --all-features` (detected `test` command is `cargo test`; CI runs
  the feature matrix in `make tests`, `Makefile:226`). The memory-cap prefix is mandatory in this
  repo.
- `cargo clippy --all-targets` (detected `lint`), then `make check` and `make examples`.
- New `tests/v3_refresh_claim.rs`, one test per behavior:
  - a second `claim` of a live key returns `None`, and succeeds again once the first is dropped;
  - the key is released on normal completion, and `registry.is_empty()`;
  - the key is released on an unwind: hold a claim inside `catch_unwind` and panic, then re-claim;
  - the key is released on cancellation: hold a claim inside a future, poll it once, drop it
    without completing, then re-claim (`tokio` is already a dev-dependency, `Cargo.toml:166-168`,
    so `JoinHandle::abort` as in the examples is also available);
  - N threads racing to claim one key yield exactly one `Some`;
  - `is_claimed` agrees with the claim's lifetime, including through a borrowed `&str`;
  - the registry drains to `len() == 0` after every case above.
- Mutation check for that file: replacing the `Drop` body with a no-op must fail the unwind, the
  cancellation, and the drain tests. A test that still passes with `Drop` gutted is not testing the
  guard.
- Doc test on the module: claim, drop, re-claim, in the shape the examples use.
- Rewrite both examples onto the shipped type and delete the two hand-rolled copies. They keep
  their existing assertions, so they go on proving the panic, abort, and (if `std::sync::Mutex` is
  chosen) poison paths against the real implementation on every CI run.

## Notes

- The two examples are the current documentation of this pattern. If the guard ships they should
  be rewritten to use it, which both shrinks them by 43 duplicated lines each and turns them into
  end-to-end coverage of the new type under a real runtime.
- Issues #147 and #228 are the tracking issues for the stale-while-revalidate request; both are
  still open, answered by `examples/stale_while_revalidate.rs` rather than by API. This does not
  close them (spawning is still the caller's), but it removes the part of the recipe that was
  gotten wrong twice.
- Related: 0047 (per-key expiry read, and the "refresh policy stays with the caller" rationale
  this record narrows to spawning), 0035 (the seeded per-key lock buckets behind `sync_writes =
  "by_key"`, the crate's existing per-key coordination primitive), 0045 (standalone type versus
  a method on an existing trait).
- If this lands it needs a statement in `specs/README.md` and a new section in whichever spec file
  covers non-store surface; there is no existing home for a standalone utility type.
