# 0027 - sync_writes default flip and revert

Status: Implemented

## Current state

`#[cached]` defaults to no write synchronization: concurrent first calls for the same key each
run the body independently and the last writer wins. This has been the behavior since 1.x and
matches Python's `functools.lru_cache` contract.

`sync_writes = "by_key"` is available as an explicit opt-in. When set, concurrent calls sharing
the same cache key serialize through a bucketed per-key lock so the body runs at most once per
key at a time. `sync_writes = "by_key"` is incompatible with `result_fallback` (mutually
exclusive; compile error). `#[once]` and `#[concurrent_cached]` are unaffected by this attribute.

## History of the flip and revert

rc.1 and rc.2 changed the default for a bare `#[cached]` to `sync_writes = "by_key"`, reasoning
that deduplication is almost always the intended behavior for a memoized function. During rc
testing this default was found to block recursive cached functions: `parking_lot` mutexes are
non-reentrant, so a cached function that calls itself (directly or through another cached function
that holds a bucket lock) deadlocked on the re-entry. Additionally, the by-key lock is held
across the full body computation, blocking readers for every call to a cold key, which
undermines the timer-driven background-refresh pattern documented in `src/macros.rs` (MACRO-1).

rc.3 reverted the default to no synchronization before any stable release shipped the changed
default. The CHANGELOG (`[3.0.0-rc.3]`) carries a dedicated note for this revert.

## Design decisions recorded here

- **Default is no synchronization.** The last-writer-wins race is acceptable for a pure
  memoization function and avoids the deadlock and reader-blocking problems of an implicit lock.
- **`sync_writes = "by_key"` is opt-in.** Callers that need single-evaluation semantics per key
  must enable it explicitly, accepting the non-reentrancy constraint.
- **`sync_writes = "disabled"` is accepted** as an explicit spelling of the no-synchronization
  default, for symmetry and self-documenting opt-out.
- **`result_fallback` forces `Disabled` when `sync_writes` is not set explicitly.** The two
  attributes serve overlapping but incompatible goals; an explicit conflict error is emitted if
  both are set with a non-Disabled sync mode.

## Notes

- The revert was made before 3.0.0 final; no stable release ever shipped the `"by_key"` default.
- The seeded per-key lock bucket hasher is documented in spec 0035.
- `cached_proc_macro/src/cached.rs:781-794` enforces the `result_fallback`/`sync_writes` conflict.
