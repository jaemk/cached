# 0032 - CachedAsync renamed to CachedGetOrSetAsync; sync passthroughs removed

Status: Implemented

## Current state

`CachedGetOrSetAsync<K, V>` is the trait that memoizes an async closure over a synchronous
in-memory `Cached` store (`src/lib.rs:1691`). It provides the get-or-set family:
`async_cache_get_or_set_with`, `async_cache_get_or_set_with_mut`,
`async_cache_try_get_or_set_with`, `async_cache_try_get_or_set_with_mut`. It does not expose
`async_cache_get`, `async_cache_set`, `async_cache_remove`, or `async_cache_clear`.

For a fully async store with its own synchronization (`RedbCache`, `AsyncRedisCache`, the
sharded in-memory stores), the correct trait is `ConcurrentCachedAsync`, which mirrors the full
sync surface. `CachedGetOrSetAsync` is explicitly not for those stores.

## History of the rename

The trait was called `CachedAsync` through rc.2. That name implied parity with `Cached` (a full
async mirror of the sync trait), but the trait's only real job is providing get-or-set for an
async closure. The four sync passthroughs (`async_cache_get` etc.) that forwarded to the
underlying `Cached` methods were removed alongside the rename: they added no value (callers could
call the sync methods directly), and the `Self: Cached` supertrait bound they relied on is
architecturally wrong for a trait whose purpose is composing with, not replacing, `Cached`.

The rename and removal landed in rc.3. Migration: replace `cached::CachedAsync` imports with
`cached::CachedGetOrSetAsync`; call `cache_get`/`cache_set`/`cache_remove`/`cache_clear` directly
on an in-memory `Cached` store instead of the removed async wrappers.

## Design decisions recorded here

**The trait is narrowly scoped.** `CachedGetOrSetAsync` does one thing: it adds an async factory
to the synchronous get-or-set pattern. In-memory stores have no async I/O; making them `async` is
only useful when the factory (the value-computing closure) is async. `ConcurrentCachedAsync` is
the right bound for code that needs a general async cache surface.

**The `async_` prefix on method names is load-bearing.** Callers commonly import both `CachedExt`
(for short sync aliases like `get`/`set`) and `CachedGetOrSetAsync`. Without the `async_` prefix,
`get_or_set_with` would be ambiguous between the two traits. The prefix eliminates the ambiguity
without requiring disambiguating syntax (`<MyStore as CachedGetOrSetAsync<K,V>>::...`).

**`Self: Cached` supertrait was removed.** The removed bound forced any implementor of
`CachedGetOrSetAsync` to also implement `Cached`. It was overly restrictive for a narrow-purpose
trait and inconsistent with `ConcurrentCachedAsync`, which does not require `ConcurrentCached`
as a supertrait.

## Notes

- The trait is gated on `feature = "async_core"` (`src/lib.rs:1689`).
- `src/lib.rs:1678-1688` documents the scope distinction between `CachedGetOrSetAsync` and
  `ConcurrentCachedAsync` in the trait rustdoc.
