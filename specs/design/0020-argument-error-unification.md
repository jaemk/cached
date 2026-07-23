# 0020 - Unify single-variant argument errors

Status: Declined (split kept; `CacheSetError` removed instead)

## Current state

- Two single-variant error enums for bad setter arguments: `SetMaxSizeError::ZeroMaxSize`,
  `SetTtlError::ZeroTtl` (`src/stores/mod.rs`).
- Both are "you passed a bad argument at a setter".
- `CacheSetError::TimeBounds` was removed for 3.0.0: TTL stores treat an
  `Instant`-overflowing expiry as never-expires (matching the sharded stores), so
  the unsync TTL stores are `Cached::Error = Infallible` and no set path fails.

## Desired work

- None. Merging the remaining two was considered and declined.

## Notes

- The per-operation single-variant typing is precise (a try_set_max_size can only
  fail one way); keeping them split was the conscious 3.0 decision. Both are
  #[non_exhaustive].
