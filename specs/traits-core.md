# Core cache traits

The single-owner cache trait family, defined in `src/lib.rs` and re-exported at the crate root.
These take `&mut self` (exclusive ownership), distinguishing them from the concurrent family in
[traits-concurrent.md](traits-concurrent.md).

## TRAIT-1

`Cached<K, V>` is the core: `cache_get`, `cache_get_mut`, `cache_set`, `cache_get_or_set_with`
(and `_mut` / `try_` variants), `cache_remove`, `cache_remove_entry`, `cache_delete`,
`cache_clear`, `cache_reset`, `cache_size`, and the metric accessors (`cache_hits` /
`cache_misses` / `cache_capacity` / `cache_evictions`). `CachedExt` is a blanket extension trait
providing the deduplicated method names and `metrics()`, per
[design/0008-method-name-deduplication.md](design/0008-method-name-deduplication.md).

## TRAIT-2

`CachedPeek<K, V>` provides `cache_peek` (non-mutating, skips recency/TTL refresh and metrics).
`CachedRead<K, V>: CachedPeek` adds `cache_get_read` for shared-ref reads (backs `unsync_reads`).
Merging `CachedPeek` / `CachedRead` is an open direction
([design/0023-peek-read-trait-merge.md](design/0023-peek-read-trait-merge.md)).

## TRAIT-3

`CachedIter<K, V>` iterates entries (filtering expired ones without removing them).
`CloneCached<K, V>` returns owned values with expiry status (`cache_get_with_expiry_status`,
`cache_peek_with_expiry_status`). `CacheTtl` provides `ttl()` / `set_ttl()` / `unset_ttl()` /
`try_set_ttl()` / `refresh_on_hit()` / `set_refresh_on_hit()` on single-owner timed stores.

## TRAIT-4

`CacheEvict` provides `evict() -> usize` to sweep expired entries (firing `on_evict`); see
[builders.md](builders.md). `Expires` is implemented by values in the expiring stores
(`is_expired()`); see [store-expiring.md](store-expiring.md). Whether `Cached::get` should take
`&self` is an open direction
([design/0009-cached-get-shared-receiver.md](design/0009-cached-get-shared-receiver.md)).
