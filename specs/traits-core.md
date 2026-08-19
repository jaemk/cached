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
