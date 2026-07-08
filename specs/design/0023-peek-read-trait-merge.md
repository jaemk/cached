# 0023 - Merge CachedPeek/CachedRead; trait fragmentation

Status: Declined (DEC-5=B)

## Current state

- The public trait surface is ~16 traits.
- `CachedRead` (`src/lib.rs:1414`) adds exactly one method that defaults to calling
  `CachedPeek::cache_peek` (`src/lib.rs:1399`).
- Basic operations on a TTL store can require importing several traits (Cached + CacheTtl +
  CacheEvict + CloneCached). The prelude re-exports 14 traits.

## Desired work

- Merge CachedPeek + CachedRead into one trait.
- Consider folding CacheEvict into CacheTtl and ConcurrentCacheEvict into ConcurrentCacheTtl,
  since every store that has one has the other. Shrinks the prelude and the per-store import
  count.

## Notes

- The Peek/Read merge is nearly free (one delegates to the other).
- Folding Evict into Ttl couples a TTL knob with a sweep method; given the actual store set that
  coupling is fine. Related: 0009.

## Decision

DEC-5=B: merge declined. `CachedRead` is kept as a distinct compile-time marker trait.
It gates `unsync_reads` on custom `ty` stores: the macro requires the store to implement
`CachedRead`, not just `CachedPeek`, because a peek (non-mutating, skips recency/TTL) is
not always a valid shared-lock read. Folding into `CachedPeek` would remove that
enforcement and allow stores with LRU recency updates to be used under a shared read
lock incorrectly.
