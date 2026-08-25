# 0009 - Cached::get taking &self

Status: Needs research

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
