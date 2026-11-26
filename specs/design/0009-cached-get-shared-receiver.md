# 0009 - Cached::get taking &self

Status: Declined

## Current state

- `Cached::cache_get`/`get` take `&mut self` (`src/lib.rs:1289`), as do `cache_get_mut`
  (`:1295`), `contains`, and `CloneCached::cache_get_with_expiry_status`.
- Justified by LRU recency updates, TTL refresh, and hit/miss metrics mutating on read.
- `CachedPeek::cache_peek` (`&self`, `src/lib.rs:1888`) and `CachedRead::cache_get_read`
  (`&self`) exist as shared-borrow escape hatches.

## Scope: this is narrower than "supporting references"

Issue #64 is filed as "supporting references", which is broader than what this record can
deliver. Keeping the two apart:

- `cache_get` already RETURNS `Option<&V>`. Only its receiver is `&mut self`. So a reference
  out of a single-owner in-memory store works today, and `cache_peek` gives the same thing
  from `&self`. This record is about the receiver on the mutating read, nothing else.
- Through the macros, returning a reference is impossible regardless of what this record
  does. The generated wrapper holds the cache static's lock guard in a local and returns the
  user's declared type, so a `&V` borrowed from that guard cannot escape the wrapper. The
  documented answer for macro users is to return `Arc<T>` (`src/lib.rs` crate doc), pinned by
  `returning_arc_hands_back_the_same_allocation_on_a_hit` in `tests/v3_macros.rs`.
- The sharded stores hit the same guard problem once per shard, and the IO stores deserialize
  on read, so there is no stored `&V` to borrow at all. Design 0040 already settled that as
  "peek is an in-memory concept". Neither is reachable from this record.

So even if this landed in full, `#[cached]` users would see no change. That is worth knowing
before anyone prices the work off issue #64's title.

## Desired work

- Move hit/miss counters to Cell/atomics and LRU recency to interior mutability so the core
  `get` could take `&self`, matching user intuition.
- If it lands, fold away CachedPeek/CachedRead.

## Notes

- Deferred as too invasive for now. Highest-impact ergonomic change but real engineering cost
  and a possible borrow-panic surface for RefCell-based LRU recency.
- Revisit deliberately; do not bundle into the current release. Related: 0023.

## Research (4.1.1)

What a `&self` `cache_get` would have to move behind interior mutability, store by store:

| Mutation on read | Stores | Interior-mutable option |
|---|---|---|
| Hit/miss counters | all | Already done: `AtomicU64` or `StripedCounter` |
| LRU recency (move to MRU) | `LruCache`, `LruTtlCache`, `ExpiringLruCache` | Lock or deferred access buffer |
| Refresh-on-hit deadline | `TtlCache`, `LruTtlCache` (runtime flag) | Atomic deadline per entry |
| Sweep an expired entry, fire `on_evict`, bump `evictions` | `TtlCache`, `ExpiringCache`, `LruTtlCache`, `ExpiringLruCache` | Defer the sweep to the next `&mut` op |

The counters were the easy part and are already interior: `UnboundCache`, `HashMap`, and
`TtlSortedCache` implement `CachedRead::cache_get_read` (`&self`, counts hits and misses) today.

The rest does not have a good option:

- **`RefCell`/`Cell` for recency** makes the store `!Sync`. `RwLock<LruCache>` stops being
  `Sync`, which breaks every user who shares a store behind `Arc<RwLock<_>>`, and it turns a
  borrow error into a runtime panic.
- **A `Mutex` inside the store** keeps it `Sync`, but every macro-generated static already wraps
  the store in a `Mutex`/`RwLock`, so each hit takes two locks. A `&self` read under the outer
  read lock still serializes on the inner one, so `unsync_reads` gains nothing for LRU.
- **A deferred access buffer** (moka/Caffeine style: record hits in a lossy concurrent buffer,
  replay them on the next write) keeps `&self` cheap but makes LRU order approximate. Exact
  recency is part of `LruCache`'s contract (design 0038, `cache_set` promotes on overwrite), and
  `moka` already covers the approximate design (`examples/moka_custom_store.rs`).
- **An atomic deadline** for refresh-on-hit is feasible (nanoseconds since a store-local
  `Instant` in an `AtomicU64`), but it only helps `TtlCache`, and it adds an atomic RMW to every
  refreshing hit for no gain under the macros' exclusive lock.
- **Deferring the expired-entry sweep** changes when `on_evict` fires and when `evictions` is
  counted. That is acceptable for an opt-in read path (it is what `TtlSortedCache`'s
  `CachedRead` already does) but not for `cache_get` itself.

## Decision

Keep `Cached::cache_get(&mut self)`. A `&self` core read either breaks `Sync`, doubles the
locking, or makes LRU approximate, and none of it is visible to `#[cached]` users (see
"Scope" above). `CachedPeek` and `CachedRead` stay as the shared-borrow paths (0023).

Possible follow-up, not scheduled: `CachedRead` for `ExpiringCache`. It has no refresh-on-hit
and no recency, so a `&self` read only needs the deferred sweep, following the `TtlSortedCache`
precedent. That would let `#[cached(unsync_reads = true)]` accept it. `TtlCache` cannot get the
same impl because refresh-on-hit is a runtime flag, not a type-level one.
