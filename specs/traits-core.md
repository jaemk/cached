# Core cache traits

The single-owner cache trait family, exported at the crate root (most are defined in
`src/lib.rs`; `CacheEvict` and `Expires` are defined under `src/stores/` and re-exported).
These take `&mut self` (exclusive ownership), distinguishing them from the concurrent family in
[traits-concurrent.md](traits-concurrent.md).

## TRAIT-1

`Cached<K, V>` is the core: `cache_get`, `cache_get_mut`, `cache_set`, `cache_try_set`,
`cache_get_or_set_with` (and `_mut` / `try_` variants), `cache_remove`, `cache_remove_entry`,
`cache_delete`, `cache_clear`, `cache_reset`, `cache_size`, `cache_contains` (defaulted; built-ins
override with a peek-based implementation; the trait-level default is get-based for third-party
stores), and the metric accessors (`cache_hits` / `cache_misses` / `cache_capacity` /
`cache_evictions`). `Cached::Error` is bounded by `std::error::Error + Send + Sync + 'static` so
generic callers can `?`-propagate or `.unwrap()` without extra where-clauses. `CachedExt` is a
blanket extension trait providing deduplicated short-name methods (`get`, `get_mut`, `set`,
`try_set`, `get_or_set_with`, `get_or_set_with_mut`, `try_get_or_set_with`,
`try_get_or_set_with_mut`, `remove`, `remove_entry`, `delete`, `contains` (delegates to
`cache_contains`), `clear`, `reset`, `len`, `is_empty`, `hits`, `misses`, `capacity`,
`evictions`, `metrics()`), per
[design/0008-method-name-deduplication.md](design/0008-method-name-deduplication.md).

## TRAIT-2

`CachedPeek<K, V>` provides `cache_peek` (non-mutating, skips recency/TTL refresh and metrics)
and a `peek` alias. `CachedRead<K, V>: CachedPeek` adds `cache_get_read` for shared-ref reads
(backs `unsync_reads`). A `CachedPeek` / `CachedRead` merge was considered and declined; they stay
distinct on purpose (`CachedRead` is a compile-time capability marker), per
[design/0023-peek-read-trait-merge.md](design/0023-peek-read-trait-merge.md). The concurrent
family's sync peek trait, `ConcurrentCachePeek`, now also has an async mirror,
`ConcurrentCachePeekAsync`; see
[traits-concurrent.md](traits-concurrent.md) CTRAIT-5.

## TRAIT-3

`CachedIter<K, V>` iterates entries (filtering expired ones without removing them).
`CloneCached<K, V>` returns owned values with expiry status (`cache_get_with_expiry_status` /
`get_with_expiry_status`, `cache_peek_with_expiry_status` / `peek_with_expiry_status`). `CacheTtl`
provides `ttl()` / `set_ttl()` / `try_set_ttl()` / `unset_ttl()` on single-owner timed stores;
refresh-on-hit is a separate trait, see TRAIT-5.

## TRAIT-4

`CacheEvict` provides `evict() -> usize` to sweep expired entries (firing `on_evict`); see
[builders.md](builders.md). `Expires` is implemented by values in the expiring stores:
`is_expired()` (required) and `expires_at() -> Option<Instant>` (provided default: `None`);
see [store-expiring.md](store-expiring.md). Whether `Cached::get` should take
`&self` is an open direction
([design/0009-cached-get-shared-receiver.md](design/0009-cached-get-shared-receiver.md)).

## TRAIT-5

`CacheRefreshOnHit` provides `refresh_on_hit()` / `set_refresh_on_hit()` on the single-owner
timed stores that can extend an entry's deadline on read: `TtlCache` and `LruTtlCache`. It was
split out of `CacheTtl`, which keeps `ttl()` / `set_ttl()` / `try_set_ttl()` / `unset_ttl()`.

`TtlSortedCache` implements `CacheTtl` but not `CacheRefreshOnHit`. Its entries live in a
deadline-ordered index, so moving one entry's expiry forward on a read would leave that index
unsorted; the store has no refresh-on-hit mode. While the capability was part of `CacheTtl` it
satisfied the bound with a `set_refresh_on_hit` that ignored its argument and returned `false`,
which a generic caller cannot distinguish from "the flag was already off". Every remaining
implementor honours the documented contract: the setter returns the state the store was actually
in, and the new state takes effect for subsequent hits.

## TRAIT-6

`CacheExpiry<K, V>` provides `cache_peek_expires_at` (and a defaulted `peek_expires_at` alias), a
side-effect-free per-key expiry read returning `(Option<V>, Option<Instant>)`: `(None, None)` when
the key is absent, `(Some(v), None)` when present with no known deadline, and `(Some(v), Some(t))`
otherwise, where a past `t` means the entry is already expired (it is returned rather than
removed, the same as `cache_peek_with_expiry_status` returning `(Some(v), true)`; TRAIT-3). It
returns `Instant` rather than a remaining `Duration`, since a `Duration` cannot distinguish "never
expires" from "already expired". The read does no LRU promotion, hit/miss counting, TTL renewal,
or lazy removal. This answers GitHub issue #91 (refresh when the remaining TTL drops below a
threshold): a threshold predicate needs a deadline, not just the `bool` that
`cache_peek_with_expiry_status` returns.

`CacheExpiry` also provides `cache_expires_at` (and a defaulted `expires_at` alias), a value-free
companion returning `(bool, Option<Instant>)`: the `bool` is presence and the `Option<Instant>` is
the deadline, so `(false, None)` is absent, `(true, None)` is present with no deadline, `(true,
Some(t))` with a future `t` is live, and `(true, Some(t))` with a past `t` is expired and not
removed. The presence flag is deliberate: absent and never-expires call for opposite actions in a
threshold-refresh policy (a cold fetch versus doing nothing), so collapsing them into a bare
`Option<Instant>` would repeat the mistake a remaining `Duration` already makes for the deadline
itself. The tuple shape matches the existing `(Option<V>, bool)` and `(Option<V>, Option<Instant>)`
returns in this family. `cache_expires_at` carries no `V: Clone` bound: that bound moved off this
trait's impl blocks and onto the value-returning methods (`cache_peek_expires_at` and its
`peek_expires_at` alias), so a deadline-only read works for a value type that does not implement
`Clone`. It shares the same side-effect-free contract, and the same advisory-deadline caveat on
the `Expires`-based stores, as `cache_peek_expires_at` below.

Deliberately a standalone trait rather than a new required method on `CloneCached`: that would
break external store implementations. Implemented by `TtlCache`, `LruTtlCache`, `TtlSortedCache`,
`ExpiringCache`, and `ExpiringLruCache`. On the last two, whose deadline comes from
`Expires::expires_at()` (TRAIT-4), the returned `Instant` is advisory: `expires_at()`'s default
body returns `None`, so the pair can be `(Some(v), None)` for an entry that is in fact expired.
The two can disagree in both directions: `t` can be in the past for an entry `Expires::is_expired`
reports live, and `t` can be in the future for an entry `is_expired` reports expired (a token with
a fixed deadline that is also revocable). A future `t` is therefore not evidence that the entry is
live on these stores. `cache_peek_with_expiry_status` remains the authoritative liveness read on
those two stores. The concurrent mirror is `ConcurrentCacheExpiry`, see
[traits-concurrent.md](traits-concurrent.md) CTRAIT-9.

## TRAIT-7

`CacheSetMaxSize` provides `set_max_size(&mut self, max_size: usize) -> Option<usize>` (returns
the previous capacity; panics on `max_size == 0`) and `try_set_max_size(&mut self, max_size:
usize) -> Result<Option<usize>, SetMaxSizeError>` (returns `Err(SetMaxSizeError::ZeroMaxSize)`
instead of panicking). Both methods already existed as inherent-only methods on every implementor;
the trait adds no new capability, only a route reachable from generic code holding a `T:
CacheSetMaxSize` bound rather than a concrete store type. Shrinking evicts eagerly, firing
`on_evict` and counting an eviction per removed entry, matching the pre-existing inherent
behavior. Implemented by `LruCache`, `LruTtlCache`, `ExpiringLruCache`, and `TtlSortedCache`, the
four single-owner stores with a live, resizable capacity. Not implemented by the unbounded stores
(`UnboundCache`, `TtlCache`, `ExpiringCache`), which have no capacity field to resize, nor by the
IO stores (`RedisCache`, `AsyncRedisCache`, `RedbCache`), which have no client-side capacity.
`SetMaxSizeError` is `#[non_exhaustive]` and shared with the concurrent mirror,
`ConcurrentCacheSetMaxSize` ([traits-concurrent.md](traits-concurrent.md) CTRAIT-10), which adds
`SetMaxSizeError::CapacityOverflow` for a bound that overflows when divided across shards; a
single-owner implementation of `try_set_max_size` never constructs that variant. See
[design/0050-capability-traits-for-inherent-only-ops.md](design/0050-capability-traits-for-inherent-only-ops.md).

## TRAIT-8

`CacheClearWithOnEvict` provides `cache_clear_with_on_evict(&mut self)`, which clears the store
like `cache_clear()` but fires `on_evict` (and counts an eviction) for every removed entry. The
method already existed as inherent-only on every single-owner in-memory store; the trait adds no
new capability, only the generic-code route. Unlike TRAIT-7, coverage is total, not partial: all
seven single-owner in-memory stores implement it (`UnboundCache`, `LruCache`, `TtlCache`,
`LruTtlCache`, `TtlSortedCache`, `ExpiringCache`, `ExpiringLruCache`), since every one of them has
an `on_evict` callback. The single-owner/concurrent split still exists for receiver-family
symmetry with `ConcurrentCacheClearWithOnEvict`
([traits-concurrent.md](traits-concurrent.md) CTRAIT-11), not because coverage differs. The IO
stores (`RedisCache`, `AsyncRedisCache`, `RedbCache`) have no `on_evict` mechanism and implement
neither trait. See
[design/0050-capability-traits-for-inherent-only-ops.md](design/0050-capability-traits-for-inherent-only-ops.md).
