# TTL caches

Time-based single-owner stores, gated behind the `time_stores` feature: `TtlCache` (global TTL,
unbounded), `LruTtlCache` (LRU + global TTL, size-bounded), and `TtlSortedCache` (TTL-ordered,
optional size limit). Renamed from the pre-1.0 `TimedCache` / `TimedSizedCache` /
`ExpiringSizedCache`.

## TTL-1

All three apply a single global TTL to every entry. A `cache_get` on an expired entry misses and
removes it. `TtlCache` and `LruTtlCache` refresh the stored value's timestamp on hit when
`refresh_on_hit` is set; `TtlSortedCache` is ordered by expiry.

## TTL-2

Constructors: infallible `TtlCache::new(ttl)` and `LruTtlCache::new(max_size, ttl)`, plus the
`::builder()` form for each. TTL is set as a `Duration`; the macros accept `ttl_secs`,
`ttl_millis`, or `ttl = "<Duration expr>"`. See [macro-cached.md](macro-cached.md).

## TTL-3

These stores implement `CacheTtl` (`ttl()` / `set_ttl()` / `unset_ttl()`). `set_ttl(0)` and the
per-entry expiry model follow
[design/0028-per-entry-expiry-and-set-ttl-zero.md](design/0028-per-entry-expiry-and-set-ttl-zero.md).

## TTL-4

`TtlSortedCache` implements `CachedRead` (shared-ref reads / `unsync_reads`); `TimedEntry<V>` is
`pub(crate)` and not part of the public surface. Size/iter/evict semantics follow
[design/0002-size-iter-evict-semantics.md](design/0002-size-iter-evict-semantics.md).
