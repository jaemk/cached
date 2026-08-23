# 0047 - Per-key expiry read: `CacheExpiry` / `ConcurrentCacheExpiry`

Status: Implemented

## Previous state

`CloneCached::cache_peek_with_expiry_status` and its `ConcurrentCloneCached` counterpart return
`(Option<V>, bool)`. The `bool` says expired-or-not and nothing more, so a caller cannot express
"remaining TTL is under 10 seconds, refresh now" - the question raised in issue #91
("Auto-refresh when remaining TTL is below a certain threshold", open since 2021).

The only per-entry deadline already on the 3.0 public surface was `LruTtlCache::iter_order()`
paired with `CacheValue::expires_at()`, which clones the whole store and requires
`K: Clone + V: Clone`. Plain `TtlCache` has no equivalent, and the sharded stores have no
enumeration at all - they deliberately do not implement `CachedIter` - so `#[concurrent_cached]`
users had no route to this at all.

## Decision

Two new standalone public traits, mirroring the `CloneCached` / `ConcurrentCloneCached` split:

```rust
pub trait CacheExpiry<K, V> {           // single-owner stores, &self, Borrow<Q> key
    fn cache_peek_expires_at<Q>(&self, key: &Q) -> (Option<V>, Option<Instant>);
    fn peek_expires_at<Q>(&self, key: &Q) -> (Option<V>, Option<Instant>);  // defaulted alias
}
pub trait ConcurrentCacheExpiry<K, V> { // sharded stores, &self, &K
    fn cache_peek_expires_at(&self, key: &K) -> (Option<V>, Option<Instant>);
    fn peek_expires_at(&self, key: &K) -> (Option<V>, Option<Instant>);     // defaulted alias
}
```

`Instant` is `crate::time::Instant`, the wasm-aware `web_time` re-export already used throughout
the crate. Implemented by `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`,
`ExpiringLruCache`, and the four sharded expiry stores (`ShardedTtlCache`, `ShardedLruTtlCache`,
`ShardedExpiringCache`, `ShardedExpiringLruCache`). Not implemented by `RedisCache`,
`AsyncRedisCache`, or `RedbCache` (they implement neither `CloneCached` nor
`ConcurrentCloneCached`), and not by the non-expiry stores.

Both traits join the crate-root exports and `cached::prelude`, matching `CloneCached` /
`ConcurrentCloneCached`.

## Rationale

**New traits, not a new required method on `CloneCached` / `ConcurrentCloneCached`.** A required
method on an existing public trait breaks every external store implementation, and this is a
post-3.0 additive release. A defaulted method returning `None` was rejected too: a default that
silently reports "no deadline" for a store that does have one is a lie the type system cannot
catch. `CacheExpiry` is also not a supertrait of `CloneCached` - that would force implementors
into an unrelated obligation for no benefit.

**`Option<Instant>`, not a remaining `Option<Duration>`.** A `Duration` cannot distinguish "never
expires" from "already expired" - both collapse to `None` or zero. `Instant` gives `None` = no
deadline, `Some(past)` = expired, `Some(future)` = live, and it is already the type
`CacheValue::expires_at()` returns, so the two per-entry deadline surfaces agree.

**`&K`, not `Borrow<Q>`, on the concurrent trait.** Same reasoning already documented on
`ConcurrentCloneCached`: the concurrent trait family includes external stores that must serialize
the key, and a `Borrow<Q>` carries no serialization guarantee.

**Value semantics on the `Expires`-based stores.** On `ExpiringCache` / `ExpiringLruCache` and
their sharded counterparts the deadline is `Expires::expires_at()`, whose default implementation
returns `None`, while `Expires::is_expired()` is the authority on liveness. The returned deadline
is therefore advisory only on those stores: `None` even for an expired entry (any value type that
does not override `expires_at`), and possibly in the past for an entry `is_expired` still reports
as live. This asymmetry is accepted rather than papered over, and
`cache_peek_with_expiry_status` stays the authoritative liveness read on those stores.

**Refresh policy stays with the caller.** The crate has no runtime dependency and cannot spawn, so
the trait reports the deadline and the caller decides when and how to call `{fn}_prime_cache`.
Same reasoning already applied to issues #147/#228 and `examples/stale_while_revalidate.rs`. A
macro-level `refresh_ahead` attribute was considered and rejected: the macro cannot spawn either,
so it could only generate a predicate over this same trait, adding attribute surface for no new
capability.

## Alternatives considered

**A required method on `CloneCached` / `ConcurrentCloneCached`.** Rejected - breaking.

**A defaulted method on `CloneCached` / `ConcurrentCloneCached` returning `None` by default.**
Rejected - a decorative default that a compliant store could silently fail to override, the same
failure mode 0023, 0040, and 0045 already rejected for other traits.

**`CacheExpiry` as a supertrait of `CloneCached`.** Rejected - couples an unrelated capability
onto every implementor.

**Remaining `Duration` instead of `Instant`.** Rejected - cannot express "no deadline" versus
"already expired" as distinct values.

**Macro-level `refresh_ahead` attribute.** Rejected - the macro has the same no-runtime
constraint as the caller, so it could only generate sugar over `peek_expires_at` plus a caller-
supplied threshold, for extra attribute surface and no new capability.

## Observable surface that changes

- New public traits `CacheExpiry` and `ConcurrentCacheExpiry`, both in `cached::prelude`.
- `TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`,
  `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, and `ShardedExpiringLruCache`
  each gain `cache_peek_expires_at` / `peek_expires_at`.
- `examples/refresh_before_expiry.rs` composes the new read with `{fn}_prime_cache`: peek the
  deadline, compare against a threshold, spawn the refresh. It is the first recipe in the crate
  that works against the sharded stores, since they have no enumeration API to fall back to.

## Notes

- Shipped statement: [traits-core.md](../traits-core.md) TRAIT-6 and
  [traits-concurrent.md](../traits-concurrent.md) CTRAIT-9.
- Answers issue #91. Does not touch #147/#228, which the stale-while-revalidate example already
  answers for the already-expired case; #91 is the before-expiry variant.
- Related: 0045 (a capability that not every implementor can honestly provide gets its own trait,
  not a defaulted method on an existing one).
