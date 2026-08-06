# 0045 - Refresh-on-hit is its own trait, on both sides

Status: Implemented

## Previous state

`CacheTtl` carried six methods: `ttl`, `set_ttl`, `try_set_ttl`, `unset_ttl`, `refresh_on_hit`,
and `set_refresh_on_hit`. `ConcurrentCacheTtl` carried the same six with `&self` receivers.
`set_refresh_on_hit` was documented as "Set whether cache hits should refresh the TTL. Returns the
previous value", and `TtlCache`, `LruTtlCache`, `RedisCache`, `AsyncRedisCache`, `RedbCache`,
`ShardedTtlCache`, and `ShardedLruTtlCache` all honoured that.

`TtlSortedCache` did not. Its impl was:

```rust
fn refresh_on_hit(&self) -> bool { false }
fn set_refresh_on_hit(&mut self, _refresh: bool) -> bool { false }
```

The argument was discarded and `false` was returned unconditionally. A generic caller bounded on
`CacheTtl` therefore could not distinguish "the flag was already off" from "this store cannot do
it": both are `false`, and the follow-up `refresh_on_hit()` reads `false` in both cases too. The
only way to learn the truth was to know the concrete type, which defeats the bound.

The no-op was not an oversight. `TtlSortedCache` keeps entries in a deadline-ordered index it
scans from the front to expire entries and to enforce `max_size`. Moving one entry's expiry
forward on a read would leave that index unsorted, so the store genuinely has no refresh-on-hit
mode and cannot grow one without changing its data structure.

## Decision

Split the capability into its own trait on both sides.

- `CacheTtl` keeps `ttl`, `set_ttl`, `try_set_ttl`, `unset_ttl`.
- `CacheRefreshOnHit` takes `refresh_on_hit(&self) -> bool` and
  `set_refresh_on_hit(&mut self, bool) -> bool`. Implemented by `TtlCache` and `LruTtlCache`.
  `TtlSortedCache` does not implement it; the no-op impl is deleted.
- `ConcurrentCacheTtl` keeps `ttl`, `set_ttl`, `try_set_ttl`, `unset_ttl`.
- `ConcurrentCacheRefreshOnHit` mirrors the sync trait with `&self` receivers. Implemented by
  `RedisCache`, `AsyncRedisCache`, `RedbCache`, `ShardedTtlCache`, `ShardedLruTtlCache`.

Both traits are ungated, matching `CacheTtl` / `ConcurrentCacheTtl`, so an external store can
implement either without `time_stores`. Both join the crate-root exports and `cached::prelude`.
Builder `.refresh_on_hit(bool)` setters are unchanged: they configure the store at construction
and never went through the trait.

This is a breaking change and lands in 3.0. Adding a trait and moving methods off an existing one
cannot be done in a 3.x patch.

## Rationale

Records 0023 and 0040 both settle the same question: a trait bound that enforces nothing is not
worth having. 0023 declined merging `CachedPeek` into `CachedRead` because `CachedRead` is a
compile-time capability marker that gates `unsync_reads`, and folding it away would let stores
with LRU recency updates pass a bound they do not satisfy. 0040 chose a separate
`ConcurrentCachePeekAsync` over a defaulted `async_cache_peek` on `ConcurrentCachedAsync` because
a default body could only be written as an ordinary get, satisfying the signature while violating
the contract, leaving the bound decorative.

Refresh-on-hit under `CacheTtl` was the same failure in its most literal form: a required method
whose contract one implementor could not meet, satisfied by returning a constant. The fix is the
same fix. A store that names `CacheRefreshOnHit` in its impl list can refresh on hit; the
compiler enforces the claim, and generic code that needs the knob fails to compile against
`TtlSortedCache` instead of silently doing nothing at runtime.

## Alternatives considered

**A runtime `supports_refresh_on_hit() -> bool` flag on `CacheTtl`.** Rejected. It keeps the
useless method on the trait and adds a second one to interrogate it, so callers must now write a
branch to find out whether the first method does anything. The compiler still cannot help, and
the failure mode moves from "silently does nothing" to "silently does nothing unless you
remembered to check". This is exactly the decorative bound 0023 and 0040 rejected.

**Change `set_refresh_on_hit` to return `Result` or `Option`.** Rejected. It makes every honest
implementor pay for one store's incapacity at every call site, and the incapacity is static, so
encoding it in a runtime value is the wrong axis.

**Split only the single-owner side.** Rejected. See below.

## Why the concurrent side splits too

The concurrent split discriminates nothing today. Every store implementing `ConcurrentCacheTtl`
also implements `ConcurrentCacheRefreshOnHit`; the two implementor sets are identical, and no
concurrent store is in the position `TtlSortedCache` occupies. Splitting anyway is a deliberate
choice for three reasons:

1. The two families are documented as mirrors of each other, and their trait surfaces are
   maintained in parallel (`CacheTtl` / `ConcurrentCacheTtl`, `CachedPeek` /
   `ConcurrentCachePeek`, `ConcurrentCachePeekAsync` added in 0040 for exactly this reason).
   A bound that means "TTL plus refresh" on one side and "TTL only" on the other is a trap for
   code being ported between them.
2. Both sides are frozen at 3.0. Splitting `ConcurrentCacheTtl` later is as breaking as splitting
   it now, so declining now means declining until 4.0.
3. It reserves the position: a concurrent store with a global TTL that cannot refresh on hit can
   implement `ConcurrentCacheTtl` alone rather than repeating the constant-returning stub.

The cost is one extra import for concurrent callers who use the knob, which `cached::prelude`
absorbs.

## Observable surface that changes

- `CacheTtl::refresh_on_hit` / `set_refresh_on_hit` and their `ConcurrentCacheTtl` counterparts
  no longer exist. Method-call syntax (`cache.refresh_on_hit()`) keeps working once the new trait
  is in scope; fully-qualified calls (`CacheTtl::refresh_on_hit(&cache)`) must name
  `CacheRefreshOnHit` / `ConcurrentCacheRefreshOnHit` instead.
- `TtlSortedCache` has no `refresh_on_hit` or `set_refresh_on_hit` at all. Code that called them
  was getting a no-op and now gets a compile error.
- External implementors of `CacheTtl` / `ConcurrentCacheTtl` must move their two refresh methods
  into a separate impl block for the new trait.
- New public traits `CacheRefreshOnHit` and `ConcurrentCacheRefreshOnHit`, both in the prelude.

## Notes

- Shipped statement: [traits-core.md](../traits-core.md) TRAIT-5 and
  [traits-concurrent.md](../traits-concurrent.md) CTRAIT-6.
- Related: 0023 (a capability marker is worth keeping distinct) and 0040 (a separate trait over a
  defaulted method, for the same "the bound must imply the behavior" reason).
