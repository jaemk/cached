# 0020 - Unify single-variant argument errors

Status: Needs research

## Current state

- Three single-variant error enums for bad setter arguments: `SetMaxSizeError::ZeroSize`,
  `SetTtlError::ZeroTtl`, `CacheSetError::TimeBounds` (`src/stores/mod.rs`).
- All are "you passed a bad argument at a setter".

## Desired work

- Merge into one small enum (e.g. `ValueError` with `ZeroSize | ZeroTtl | TimeBounds`), reducing
  the number of public error types.

## Notes

- Counter-argument: the current per-operation single-variant typing is precise (a
  try_set_max_size can only fail one way). Lean toward keeping them split.
- Tracked as a conscious 3.0 decision. All are already #[non_exhaustive].
