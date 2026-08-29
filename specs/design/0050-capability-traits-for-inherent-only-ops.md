# 0050 - Capability traits for `set_max_size` and `cache_clear_with_on_evict`

Status: Implemented

## Current state

Two capabilities exist only as inherent methods on concrete store types. Generic code holding a
`T: Cached<K, V>` or `T: ConcurrentCached<K, V>` cannot reach either one; only code that names the
concrete store type can.

### 1. `set_max_size` / `try_set_max_size`

Implemented by 7 of the 13 in-memory stores (the LRU-bounded family plus the deadline-ordered
`TtlSortedCache`), each as an inherent-only pair with no trait behind it:

Single-owner (`&mut self`, panics on `max_size == 0`):
- `LruCache::set_max_size` / `try_set_max_size` - `src/stores/lru.rs:309`, `:328`
- `LruTtlCache::set_max_size` / `try_set_max_size` - `src/stores/lru_ttl.rs:570`, `:584`
- `ExpiringLruCache::set_max_size` / `try_set_max_size` - `src/stores/expiring_lru.rs:330`, `:341`
- `TtlSortedCache::set_max_size` / `try_set_max_size` - `src/stores/ttl_sorted.rs:509`, `:527`

Sharded (`&self`, same panic):
- `ShardedLruCache::set_max_size` / `try_set_max_size` - `src/stores/sharded/lru.rs:498`, `:521`
- `ShardedLruTtlCache::set_max_size` / `try_set_max_size` - `src/stores/sharded/lru_ttl.rs:604`,
  `:627`
- `ShardedExpiringLruCache::set_max_size` / `try_set_max_size` -
  `src/stores/sharded/expiring_lru.rs:558`, `:581`

Signatures (identical shape on both receiver families, only the receiver differs):

```rust
pub fn set_max_size(&mut self, max_size: usize) -> Option<usize>;      // single-owner
pub fn set_max_size(&self, max_size: usize) -> Option<usize>;          // sharded
pub fn try_set_max_size(&mut self, max_size: usize)
    -> Result<Option<usize>, SetMaxSizeError>;                          // single-owner
pub fn try_set_max_size(&self, max_size: usize)
    -> Result<Option<usize>, SetMaxSizeError>;                          // sharded
```

Both return the previous capacity wrapped in `Some` (there is no `None` case in the current
implementations - it is not a sentinel for "unbounded", every store that has the method has a
`usize` capacity to report). `set_max_size` panics on `max_size == 0`; the sharded variant also
panics if `max_size` is close enough to `usize::MAX` that dividing it across the shard count and
multiplying back overflows (see `SetMaxSizeError::CapacityOverflow` doc,
`src/stores/mod.rs:210-219`). `try_set_max_size` validates first and returns
`Err(SetMaxSizeError::ZeroMaxSize)` / `Err(SetMaxSizeError::CapacityOverflow)` instead of
panicking; on the sharded stores the overflow check happens before the panicking path is reached
(`src/stores/sharded/lru.rs:521-530`).

`SetMaxSizeError` is a `#[non_exhaustive]` public enum at `src/stores/mod.rs:207-220`
(`ZeroMaxSize`, `CapacityOverflow`), already exported at the crate root
(`src/lib.rs:891`, in the same `pub use stores::{...}` block as `LruCache`). It is not currently
in `cached::prelude` - none of the crate's other error types (`SetTtlError`, `BuildError`,
`RedbCacheError`, `RedisCacheError`) are either, so this is consistent with existing practice, not
an oversight to fix here.

**Not implemented, and cannot be, without changing the store's shape:**
- `UnboundCache` / `ShardedUnboundCache` - no capacity field; the store is unbounded by
  definition. (`src/stores/unbound.rs`, `src/stores/sharded/unbound.rs`.)
- `TtlCache` / `ShardedTtlCache` - TTL-only, sized only by what has not yet expired; grep confirms
  no `max_size`/`capacity` field, only an `initial_capacity` *allocation hint* consumed once at
  build time (`src/stores/ttl.rs:139-140`, `:216-219`). Not the same thing as a live-adjustable
  cap.
- `ExpiringCache` / `ShardedExpiringCache` - same shape as `TtlCache`, confirmed the same way
  (`src/stores/expiring.rs:149-150`, `:216-217`); its rustdoc at `src/stores/expiring.rs:20,33`
  explicitly points to `ExpiringLruCache` as "the same store with a `max_size` bound" when one is
  wanted.
- `RedisCache`, `AsyncRedisCache`, `RedbCache` - no client-side capacity to mutate; capacity is a
  server/database property. Confirmed no `set_max_size`/`try_set_max_size` in `src/stores/redis.rs`
  or `src/stores/redb.rs`.

The crate doc does not mention this gap at all - `grep -n "set_max_size" src/lib.rs` returns
nothing. Unlike the second capability below, this omission is undocumented, not acknowledged.

### 2. `cache_clear_with_on_evict`

Implemented identically (return type `()`, no `Result`) by **every** in-memory store that has an
`on_evict` callback at all - all 13 built-in in-memory stores, with no partial coverage:

Single-owner (`&mut self`):
- `TtlCache` - `src/stores/ttl.rs:383`
- `UnboundCache` - `src/stores/unbound.rs:211`
- `ExpiringCache` - `src/stores/expiring.rs:301`
- `ExpiringLruCache` - `src/stores/expiring_lru.rs:432` (reported as `:431`; off by one)
- `TtlSortedCache` - `src/stores/ttl_sorted.rs:1049`
- `LruCache` - `src/stores/lru.rs:756`
- `LruTtlCache` - `src/stores/lru_ttl.rs:686`

Sharded (`&self`):
- `ShardedUnboundCache` - `src/stores/sharded/unbound.rs:347`
- `ShardedTtlCache` - `src/stores/sharded/ttl.rs:403`
- `ShardedExpiringCache` - `src/stores/sharded/expiring.rs:356`
- `ShardedExpiringLruCache` - `src/stores/sharded/expiring_lru.rs:406`
- `ShardedLruCache` - `src/stores/sharded/lru.rs:361`
- `ShardedLruTtlCache` - `src/stores/sharded/lru_ttl.rs:451` (reported as `:450`; off by one)

**Not implemented:** `RedisCache`, `AsyncRedisCache`, `RedbCache` - confirmed by grep that none of
the three has any `on_evict` concept at all (`grep -n "on_evict" src/stores/redis.rs
src/stores/redb.rs` returns nothing), so there is nothing for this method to clear-and-notify.

The crate doc *does* call this out, at `src/lib.rs:292-295` (the brief's `292-294` is short by one
line):

```
Note: `cache_clear` is a required method on `ConcurrentCached` (and `async_cache_clear` on
the async counterpart), with the short `clear()` alias on `ConcurrentCachedExt`, so generic
code over `ConcurrentCached` can clear. `cache_clear_with_on_evict()` is the exception: it is
inherent-only on each concrete sharded store type and is not callable through the trait.
```

That note is written from the `ConcurrentCached` section and only names the sharded half of the
gap. The identical gap exists on the single-owner `Cached` side (all 7 non-sharded stores above)
and is not mentioned anywhere in the crate doc; `grep -n "cache_clear_with_on_evict" src/lib.rs`
returns only those same two lines.

### Getter side is not a gap

`cache_capacity() -> Option<usize>` is already a required accessor on `Cached`
(`src/lib.rs:1478`) and `ConcurrentCacheBase` (`src/lib.rs:2742`), so it is already reachable
generically. Only the *setter* pair is inherent-only. This record is scoped to the setters plus
`cache_clear_with_on_evict`; it does not touch `cache_capacity`.

## Desired work

Add two independent trait pairs, one per capability, each split single-owner / concurrent by
receiver, following the `CacheTtl` / `CacheRefreshOnHit` precedent
([0045](0045-refresh-on-hit-trait-split.md)) rather than the `CacheExpiry` precedent
([0047](0047-per-key-expiry-read.md)): neither method touches `K` or `V`, so unlike `CacheExpiry`
these traits need no generic parameters at all, matching the shape of `CacheEvict` /
`ConcurrentCacheEvict` (`src/stores/mod.rs:704`, `:731`), which are also plain, non-generic,
non-generic-parameterized capability traits placed in `src/stores/mod.rs` and re-exported at the
crate root.

Recommended shape (place in `src/stores/mod.rs`, next to `CacheEvict`; re-export from
`src/lib.rs` alongside the other `pub use stores::{...}` blocks, same as `SetMaxSizeError`
already is at `src/lib.rs:891`):

```rust
pub trait CacheSetMaxSize {
    fn set_max_size(&mut self, max_size: usize) -> Option<usize>;
    fn try_set_max_size(&mut self, max_size: usize) -> Result<Option<usize>, SetMaxSizeError>;
}

pub trait ConcurrentCacheSetMaxSize {
    fn set_max_size(&self, max_size: usize) -> Option<usize>;
    fn try_set_max_size(&self, max_size: usize) -> Result<Option<usize>, SetMaxSizeError>;
}

pub trait CacheClearWithOnEvict {
    fn cache_clear_with_on_evict(&mut self);
}

pub trait ConcurrentCacheClearWithOnEvict {
    fn cache_clear_with_on_evict(&self);
}
```

Method names deliberately match the existing inherent methods exactly (same pattern as
`CacheEvict::evict`, `CacheTtl::set_ttl`, etc.). Because inherent methods take call-site priority
over trait methods on every concrete store type (documented at `src/lib.rs:259-266` for the
sharded family, and true of the single-owner family too), adding these traits changes nothing
about existing call sites on concrete types; they only add a route through a generic bound. No
`_mut`, no fully-qualified-syntax workaround is needed for existing callers.

**Implementor sets** (must match the "Current state" inventory above exactly - do not add a
defaulted no-op to any store not listed):

| Trait | Implementors |
|---|---|
| `CacheSetMaxSize` | `LruCache`, `LruTtlCache`, `ExpiringLruCache`, `TtlSortedCache` |
| `ConcurrentCacheSetMaxSize` | `ShardedLruCache`, `ShardedLruTtlCache`, `ShardedExpiringLruCache` |
| `CacheClearWithOnEvict` | `TtlCache`, `UnboundCache`, `ExpiringCache`, `ExpiringLruCache`, `TtlSortedCache`, `LruCache`, `LruTtlCache` (all 7 single-owner in-memory stores) |
| `ConcurrentCacheClearWithOnEvict` | `ShardedUnboundCache`, `ShardedTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`, `ShardedLruCache`, `ShardedLruTtlCache` (all 6 sharded stores) |

None of `RedisCache`, `AsyncRedisCache`, `RedbCache` implement any of the four; they have neither
a client-side capacity nor an `on_evict` mechanism to begin with (see "Current state").

Both trait pairs stay ungated (like `CacheTtl`/`CacheRefreshOnHit`, per the comment at
`src/lib.rs:1204-1208`): `LruCache`, `ExpiringLruCache`, and `UnboundCache` live in modules with no
`time_stores` gate (`src/stores/mod.rs:149-163`), while `LruTtlCache` and `TtlSortedCache` do
(`src/stores/mod.rs:152-153`, `:161-162`). Gating the trait itself on `time_stores` would make it
unusable for `LruCache`/`ExpiringLruCache` implementors when `time_stores` is off; the existing
pattern is to leave the trait ungated and let only the affected concrete `impl` blocks carry
`#[cfg(feature = "time_stores")]` where the store itself is gated.

**Exports to update once implemented:**
- Crate root: `pub use stores::{..., CacheClearWithOnEvict, CacheSetMaxSize, ...}` in the
  `src/lib.rs:888-913` export blocks (`ConcurrentCacheClearWithOnEvict` /
  `ConcurrentCacheSetMaxSize` alongside).
- `cached::prelude` (`src/lib.rs:1196-1216`): add all four new traits to the unconditional
  `pub use crate::{...}` lists, matching how `CacheRefreshOnHit`/`CacheTtl` are preluded at
  `src/lib.rs:1208` and `CacheExpiry`/`ConcurrentCacheExpiry` at `src/lib.rs:1198-1199`.
- Crate-doc trait overview in `src/lib.rs` (the running list of `[`Trait`]` links currently
  ending around `CacheExpiry`/`ConcurrentCacheExpiry`, `src/lib.rs:309-351`), and the note at
  `src/lib.rs:292-295` should be rewritten once `cache_clear_with_on_evict` stops being
  inherent-only - it currently states a fact that this record makes false.
- `AGENTS.md` `## Key Traits` table (`AGENTS.md:120-142`): four new rows, same format as the
  existing `CacheRefreshOnHit`/`ConcurrentCacheRefreshOnHit` and `CacheExpiry`/
  `ConcurrentCacheExpiry` rows (`AGENTS.md:139-142`), each naming its exact implementor list.
- `specs/traits-core.md`: append `TRAIT-7` (next free ID after `TRAIT-6` at
  `specs/traits-core.md:66-104`) for `CacheSetMaxSize` and `CacheClearWithOnEvict` (one section
  can cover both, or split into `TRAIT-7`/`TRAIT-8` - pick whichever keeps each entry as focused
  as the existing ones). IDs are stable and append-only; do not renumber `TRAIT-1`..`TRAIT-6`.
- `specs/traits-concurrent.md`: append `CTRAIT-10` (next free ID after `CTRAIT-9` at
  `specs/traits-concurrent.md:173-207`) for the concurrent mirrors, same append-only rule.
- `README.md` is generated from the `src/lib.rs` crate doc. Any crate-doc edit must be followed by
  `make docs/readme` to regenerate it; `make check/readme` (part of `make check`) verifies they
  stay in sync and CI will fail if `README.md` is edited by hand or left stale.

## Pitfalls

- **Do not give `cache_clear_with_on_evict` the same "who can't honor this" treatment as
  `set_max_size`.** All 13 in-memory stores implement it identically - there is no partial
  coverage to reason about, unlike `set_max_size` (7/13) or the `CacheRefreshOnHit` precedent
  (`TtlSortedCache` excluded). The single-owner/concurrent split still exists, but purely for
  receiver-family symmetry, exactly the reasoning 0045 gives for splitting
  `ConcurrentCacheRefreshOnHit` out even though its implementor set equals
  `ConcurrentCacheTtl`'s ("Why the concurrent side splits too",
  [0045](0045-refresh-on-hit-trait-split.md)): a generic caller migrating between the two families
  should not find a capability present on one side's bound and silently absent on the other's.
- **Do not add a defaulted no-op for `set_max_size` on `UnboundCache`/`TtlCache`/`ExpiringCache`
  and their sharded equivalents.** This is exactly the failure mode 0023/0040/0045 already reject:
  a required method whose contract one implementor cannot meet, satisfied by a constant return, is
  a bound the compiler can no longer enforce. If a future need arises to distinguish "capacity
  already at N" from "this store has no capacity", that is a reason to leave those stores off the
  trait entirely (as recommended here), not a reason to give them a stub.
- **Two trait pairs, not one covering both capabilities.** The implementor sets differ (4+3 stores
  for max-size vs. 7+6 for clear-with-on-evict is a strict superset, but still not equal), so one
  trait would force `TtlCache`/`ExpiringCache`/`UnboundCache` and their sharded forms to either gain
  a `set_max_size` stub (rejected above) or be excluded from `cache_clear_with_on_evict` too
  (wrong - they DO have it). Follow 0045's rule literally: each independently-satisfiable
  capability gets its own trait.
- **`try_set_max_size`'s `Result` error type is shared (`SetMaxSizeError`) across both new
  traits' single-owner and concurrent sides.** Do not invent a second error enum; `SetMaxSizeError`
  is already `#[non_exhaustive]` and already covers both the plain zero-check
  (`ZeroMaxSize`, hit by every implementor) and the sharded-only overflow case
  (`CapacityOverflow`, only ever returned by the three `Concurrent*` impls). A single-owner impl
  of `try_set_max_size` simply never constructs `CapacityOverflow`.
- **This is additive, not breaking, as long as no existing public trait gains a new required
  method.** Adding `CacheSetMaxSize`/`ConcurrentCacheSetMaxSize`/`CacheClearWithOnEvict`/
  `ConcurrentCacheClearWithOnEvict` as brand-new traits, plus new `impl` blocks on existing
  concrete types, breaks nothing for existing callers or external store implementors - they are
  free to ignore the new traits entirely. It WOULD become breaking if implemented instead as a new
  required method on `Cached`/`ConcurrentCached` (every external store implementation fails to
  compile) or on `CacheEvict`/`ConcurrentCacheEvict` (same problem, and also wrong - not every
  `CacheEvict` implementor has a capacity or is bounded). Keep them as free-standing traits.
- The reported source-line list in the originating brief covered under half of the true inventory
  (5 of 13 `cache_clear_with_on_evict` sites, 5 of 7 `set_max_size` sites) and had several
  off-by-one line numbers from drift since they were noted. Re-verify with the greps in
  "Current state" before trusting any specific line number in this record, since the source will
  have moved again by 3.2.

## Verification

No source changes were made for this record (doc-only handoff). To re-derive or re-verify the
inventory when implementation starts:

```
grep -rn "fn set_max_size\|fn try_set_max_size" src
grep -rn "fn cache_clear_with_on_evict" src
grep -n "SetMaxSizeError" src/stores/mod.rs src/lib.rs
grep -n "cache_clear_with_on_evict\|set_max_size" src/lib.rs   # crate-doc mentions
```

Once the traits and impls exist, the load-bearing tests are: (1) a generic function bounded on
`T: CacheSetMaxSize` (and the `Concurrent` variant) compiles when instantiated with each
implementor in the table above and fails to compile (or is simply inapplicable, since the bound
cannot be named) for `UnboundCache`/`TtlCache`/`ExpiringCache` and their sharded forms; (2) same
shape for `CacheClearWithOnEvict`/`ConcurrentCacheClearWithOnEvict` across all 13 in-memory
stores; (3) `try_set_max_size` through the trait returns `Err(SetMaxSizeError::ZeroMaxSize)` for
`0` and, on the three sharded implementors, `Err(SetMaxSizeError::CapacityOverflow)` for a
`max_size` near `usize::MAX` on a multi-shard cache, matching the existing inherent-method tests
(`src/stores/lru.rs:2085`, `src/stores/sharded/lru.rs` equivalents) so the trait path and the
inherent path agree. Run the crate's existing `cargo test --all-features` (prefix with
`ulimit -v 8000000` per repo convention - a corrupted LRU ring can otherwise runaway-allocate) and
`make check/readme` if any crate-doc line in `src/lib.rs` changes.

## Notes

- Related: [0045](0045-refresh-on-hit-trait-split.md) (the split-when-implementor-sets-differ
  rule this record follows for both capabilities) and [0047](0047-per-key-expiry-read.md) (the
  most recent trait pair added by this same shape of gap, single-owner/concurrent split by
  receiver, new-trait-not-new-required-method to stay non-breaking).
- Once implemented, allocate `specs/traits-core.md` `TRAIT-7` (and `TRAIT-8` if split into two
  entries) and `specs/traits-concurrent.md` `CTRAIT-10` (and `CTRAIT-11` if split) - confirm
  against the files directly before allocating, since sibling 3.2 work may have already claimed
  the next ID by the time this is picked up.

## Outcome

Implemented over exactly the implementor sets in "Desired work", with the implementor sets,
`try_set_max_size` error handling, and the additive/non-breaking shape all landing as designed.
One deviation from the "Recommended shape" above: the four traits (`CacheSetMaxSize`,
`ConcurrentCacheSetMaxSize`, `CacheClearWithOnEvict`, `ConcurrentCacheClearWithOnEvict`) are
defined directly in `src/lib.rs` (`:2644`, `:3068`, `:2717`, `:3106` respectively, as of this
writing), not in `src/stores/mod.rs` next to `CacheEvict` as recommended, and so are not
re-exported from `stores` at all - they are plain `pub trait` items in `src/lib.rs`, added to
`cached::prelude` as planned. This matches where the crate's other single-owner/concurrent
capability trait pairs already live (`CacheExpiry`/`ConcurrentCacheExpiry`, `CacheTtl`,
`CacheRefreshOnHit`/`ConcurrentCacheRefreshOnHit` are all `pub trait` items in `src/lib.rs`, not
`src/stores/mod.rs`), rather than the `CacheEvict`/`ConcurrentCacheEvict` precedent this record
cited for the recommendation. It was also inconsistent with `specs/traits-core.md:3-4`'s "most are
defined in `src/lib.rs`; `CacheEvict` and `Expires` are defined under `src/stores/` and
re-exported" framing at the time this Outcome was first written, since that framing already
correctly described where these four traits landed.

Allocated `specs/traits-core.md` `TRAIT-7` (`CacheSetMaxSize`) and `TRAIT-8`
(`CacheClearWithOnEvict`), split per the two-trait-pairs-not-one rule in "Pitfalls", and
`specs/traits-concurrent.md` `CTRAIT-10` / `CTRAIT-11` for their concurrent mirrors. Updated the
crate-doc trait overview (`src/lib.rs`) and the `AGENTS.md` Key Traits table with four new rows.
`README.md` regenerated via `make docs/readme` and verified with `make check/readme`.
