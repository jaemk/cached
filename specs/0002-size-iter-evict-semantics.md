# 0002 - `len`/`size` vs `iter` vs `evict` semantics

Status: Implemented

## Current state

- `cache_size()` / `len()` return the raw stored entry count, including expired-but-not-swept
  entries, on every lazy-eviction store: `TtlCache` (`src/stores/ttl.rs:516`), `TtlSortedCache`
  (documented inaccurate), `ExpiringCache` (`src/stores/expiring.rs:405`), `ExpiringLruCache`.
- `CachedIter::iter()` is `&self` and filters expired entries out of the yielded items without
  removing them (`src/stores/expiring.rs:434`, `src/stores/ttl.rs:531`). The trait doc already
  notes this (`src/lib.rs:1088`).
- `evict()` is `&mut self` and physically removes expired entries, updating the count
  (`src/stores/ttl.rs:250`, `src/stores/expiring.rs:204`).
- The inconsistency: `len()` counts expired while `iter().count()` does not, with no single
  documented contract tying them together.

## Desired contract

- `len`/`size`: return the current known stored size without applying eviction logic (cheap, no
  expiry scan). This is the existing behavior; keep it.
- `iter`: continues to omit expired entries from the view (it scans anyway). It does not free
  them; `iter` stays `&self`.
- `evict`: the explicit way to reclaim memory and get an accurate live count.

## Desired work

- Audit every timed/expiring store (sharded and non-sharded) to confirm the trio behaves as
  above, fixing any that deviate.
- Document the contract clearly and consistently: on `cache_size`/`len`, on `CachedIter`, and on
  `evict`, plus a short note in the crate-level "Behavioral guarantees" section. State that
  `len` may include expired entries and that `evict()` reclaims them.

## Notes

- Physical eviction during `iter` was considered and rejected: `iter` yields `(&K, &V)` borrows
  under `&self`, so removing during iteration is not possible without changing the receiver to
  `&mut self`, which would break the shared-iteration use case. If revisited, treat as a separate
  research item.
