# 0009 - Cached::get taking &self

Status: Needs research

## Current state

- `Cached::cache_get`/`get` take `&mut self` (`src/lib.rs:740,921`), as do `cache_get_mut`,
  `contains`, and `CloneCached::cache_get_with_expiry_status`.
- Justified by LRU recency updates, TTL refresh, and hit/miss metrics mutating on read.
- `CachedPeek::cache_peek` (&self) and `CachedRead::cache_get_read` (&self) exist as
  shared-borrow escape hatches.

## Desired work

- Move hit/miss counters to Cell/atomics and LRU recency to interior mutability so the core
  `get` could take `&self`, matching user intuition.
- If it lands, fold away CachedPeek/CachedRead.

## Notes

- Deferred as too invasive for now. Highest-impact ergonomic change but real engineering cost
  and a possible borrow-panic surface for RefCell-based LRU recency.
- Revisit deliberately; do not bundle into the current release. Related: 0023.
