# Unbound cache

`UnboundCache<K, V, S>` is an unbounded `HashMap`-backed store with no eviction. Exported from
`cached::stores` and re-exported at the crate root.

## UNBOUND-1

The cache never evicts on its own: entries live until `cache_remove`, `cache_clear`, or
`cache_reset`. `cache_size()` returns the entry count; there is no capacity.

## UNBOUND-2

Constructors: infallible `UnboundCache::new()` for the default configuration, and
`UnboundCache::builder()` for a custom hasher `S` (see [builders.md](builders.md)). The hasher
defaults to `DefaultHashBuilder`.

## UNBOUND-3

Implements `Cached`, `CachedPeek`, `CachedRead`, and `CachedIter`, so it supports shared-ref
reads (`unsync_reads` in the macros) and iteration. See [traits-core.md](traits-core.md).

## UNBOUND-4

Metrics track `hits`/`misses`; `evictions` is `None` (the store never evicts on its own). See
[metrics.md](metrics.md). Iteration and size/evict semantics follow
[design/0002-size-iter-evict-semantics.md](design/0002-size-iter-evict-semantics.md); custom
hasher support follows [design/0001-non-sharded-custom-hasher.md](design/0001-non-sharded-custom-hasher.md).
