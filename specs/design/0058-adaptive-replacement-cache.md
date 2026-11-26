# 0058 - Adaptive replacement cache (ARC) store

Status: Not implemented (declined)

## Request

Issue #32 asks for an ARC store: two resident lists, T1 (seen once recently) and T2 (seen at
least twice), plus two ghost lists, B1 and B2, holding the keys recently evicted from each. A
miss that hits a ghost list shifts the target size `p` of T1 toward that side, and eviction takes
from T1 or T2 depending on how `|T1|` compares with `p`.

## What it would cost here

- **Store surface.** Every bounded store in this crate ships as a single-owner store with a
  builder and `Cached`, `CachedIter`, `CachedPeek`, `CacheSetMaxSize`, `CacheClearWithOnEvict`,
  and `CachedGetOrSetAsync`, plus a sharded counterpart implementing the `ConcurrentCached`
  family, metrics, `on_evict`, and a test suite. For LRU that is about 6,400 lines
  (`src/stores/lru.rs`, `src/stores/sharded/lru.rs`, `src/lru_list.rs`). ARC needs the same set,
  with four lists instead of one.
- **Ghost memory.** B1 and B2 together hold up to `max_size` keys with no values, so the store
  keeps up to `2 * max_size` keys and needs `K: Clone` (already the LRU bound).
- **Mutation on hit.** A T1 hit moves the entry to T2, and a T2 hit moves it to T2's MRU, so ARC has
  the same `&mut self` read as LRU (0009) and cannot implement `CachedRead`.
- **Contract questions.** `set_max_size` has to resize all four lists and rescale `p`. `on_evict`
  has to fire for resident evictions but not for ghost-list trims. `cache_size` counts T1 + T2
  only. Each of these needs its own spec ID and tests.

## Alternatives

- **Use an existing crate through `ty`.** `#[cached(ty = "...", create = "...")]` and
  `#[concurrent_cached]` accept any store implementing the traits, and
  `examples/moka_custom_store.rs` already shows `moka` (TinyLFU admission, which is
  scan-resistant like ARC) wrapped as a `ConcurrentCached` store. That covers the issue's goal,
  a better hit rate than LRU on mixed recency/frequency workloads, without a new store here.
- **S3-FIFO, if a scan-resistant store is ever added.** S3-FIFO (Yang et al., SOSP 2023) uses three
  FIFO queues: small, main, and ghost. A hit only bumps a 2-bit per-entry frequency and never
  moves a list node. Its published hit rates are competitive with ARC and TinyLFU. Because a hit
  is a single counter bump, it is the one bounded policy that could take `&self` on read with an
  atomic, which is the obstacle 0009 records for LRU. It would be a better candidate than ARC
  for a new store here.

## Decision

Do not add ARC. The store surface is large, ARC keeps LRU's `&mut self` read, and the hit-rate
goal is met today by wrapping `moka` through `ty`. If a built-in scan-resistant store is wanted
later, open a new record for S3-FIFO rather than reviving this one.

## Notes

- Close #32 with a pointer to `examples/moka_custom_store.rs` and this record.
- Related: 0009 (`&self` reads), 0010 (read-optimized sharded LRU), 0038 (LRU promotion on
  overwrite).
