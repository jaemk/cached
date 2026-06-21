# 0023 - Merge CachedPeek/CachedRead; trait fragmentation

Status: Needs research

## Current state

- The public trait surface is ~16 traits.
- `CachedRead` (`src/lib.rs:1169`) adds exactly one method that defaults to calling
  `CachedPeek::cache_peek` (`src/lib.rs:1156`).
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
