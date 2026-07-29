# LRU cache

`LruCache<K, V, S>` is a size-bounded store with least-recently-used eviction. Renamed from the
pre-1.0 `SizedCache`. Exported from `cached::stores`.

## LRU-1

Bounded by `max_size`: inserting beyond capacity evicts the least-recently-used entry. A read
(`cache_get`) refreshes recency; a peek (`cache_peek`) does not.

## LRU-2

Constructors: `LruCache::new(max_size)` (returns the cache directly; panics on zero), or
`LruCache::builder().max_size(n)` for a custom hasher. `max_size` is the setter (renamed from
`.size()` in 2.0). Building with a zero/invalid size is a `BuildError`. See
[builders.md](builders.md).

## LRU-3

Eviction fires the `on_evict` callback when configured, and increments the `evictions` metric.
See [metrics.md](metrics.md).

## LRU-4

Implements `Cached`, `CachedPeek`, and `CachedIter`. Size/iter/evict semantics follow
[design/0002-size-iter-evict-semantics.md](design/0002-size-iter-evict-semantics.md).
Inherent `retain(keep)` removes entries failing the predicate (firing `on_evict` and counting
evictions); it now exists on every single-owner in-memory store. The expiry-aware stores
(`TtlCache`, `LruTtlCache`, `ExpiringCache`, `ExpiringLruCache`, `TtlSortedCache`) share the
contract but also remove expired entries regardless of the predicate; `TtlSortedCache` has BOTH
`retain(keep)` and the differently-purposed `retain_latest(count, evict) -> usize` (a size trim
keeping the N latest-expiring entries, unrelated to the predicate filter); on `UnboundCache` (no
eviction dimension) `retain` is a plain predicate filter that fires `on_evict` per removed entry
but counts no evictions.
`set_max_size(n) -> Option<usize>`
resizes a live cache (returns the previous capacity, panics on zero). `try_set_max_size(n) ->
Result<Option<usize>, SetMaxSizeError>` is the non-panicking variant.

## LRU-5

Order accessors, shared across the LRU family (`LruCache`, `LruTtlCache`, `ExpiringLruCache`):
`iter_order() -> Vec<(K, CacheValue<V, M>)>`, `key_order() -> Vec<K>`, and
`value_order() -> Vec<CacheValue<V, M>>`, all most-recently-used first. `CacheValue<V, M = ()>`
(exported at the crate root) wraps the value with per-entry metadata: `M = ()` for `LruCache`
and `ExpiringLruCache`, `M = Option<Instant>` for `LruTtlCache`, read via `expires_at()`. The
wrapper `Deref`s to `V`, exposes `value()` / `into_value()`, and compares equal against bare
values (`PartialEq<V>`).

## LRU-6

When `on_evict` fires for a displaced entry it receives the **stored** key, not the caller's key.
It does not fire on every overwrite: a plain `cache_set` overwrite on `LruCache` returns the old
value and fires no callback, and on `LruTtlCache` / `ExpiringLruCache` an overwrite fires
`on_evict` only when the displaced entry has already expired; capacity eviction of an
already-present key always fires it. The two keys compare equal (same `Hash`/`Eq`) but can differ
in fields outside `Hash`/`Eq`, and the stored key is the one callers with such key types should
see. `LruCache`, `LruTtlCache`, and
`ExpiringLruCache` are all consistent on this contract; the internal
`LruCache::cache_set_returning_entry` primitive (shared by the timed wrappers) returns the
displaced `(stored_key, stored_value)` pair for exactly this reason, and `ExpiringLruCache`'s
`cache_set` was brought onto the same contract (it previously passed the caller's key instead).

Which key SURVIVES an overwrite is a separate question from which key `on_evict` sees, and the
two store families deliberately differ. The LRU family overwrites through `LRUList::set`, which
replaces the whole `(K, V)` slot, so the caller's key REBINDS the entry and the previous stored
key is handed to `on_evict` and then dropped. The HashMap-backed stores (`ExpiringCache`,
`TtlSortedCache`) overwrite through `Entry::Occupied::insert`, which keeps the original key and
discards the caller's, matching `HashMap::insert` (see
[store-ttl.md](store-ttl.md) TTL-7). For a key type whose `Eq` ignores part of its payload, the
key left in an LRU-family cache after `cache_set` / a `get_or_set` overwrite is therefore the
most recently inserted one, while in the HashMap-backed stores it is the first-inserted one.
Key rebinding is independent of recency (LRU-7): a rebind happens on every overwrite regardless
of where the entry sits in the eviction order.

## LRU-7

`cache_set` over an EXISTING key promotes that key to most-recently-used. A write is an access,
so an overwrite moves the entry to the front of the eviction order exactly as a fresh insertion
would, and it therefore changes which entry a later capacity eviction selects. This holds across
`LruCache`, `LruTtlCache`, `ExpiringLruCache`, `ShardedLruCache`, `ShardedLruTtlCache`, and
`ShardedExpiringLruCache`, and it holds regardless of whether an `on_evict` callback is
configured: on the sharded LRU-TTL and expiring-LRU stores the callback branch (which needs the
displaced stored key, so it writes through `LruCache::cache_set_returning_entry`) and the
no-callback branch (a plain `LruCache::cache_set`) now agree, so attaching a purely
observational callback cannot change eviction order.

Reads that are documented as side-effect-free stay side-effect-free: `cache_peek`,
`cache_peek_with_expiry_status`, and `cache_contains` do NOT promote, so a write and a peek
remain distinguishable. Inserting a NEW key already went to the front and is unchanged.

Before 3.0 an overwrite replaced the value in place and left recency untouched (an artifact of
the original `SizedCache` implementation, where `LRUList::set` in place was the cheap path).
See [design/0038-cache-set-promotes-on-overwrite.md](design/0038-cache-set-promotes-on-overwrite.md).
