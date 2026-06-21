# 0019 - Drop ahash from default features

Status: Needs research

## Current state

- `default = ["proc_macro", "ahash", "time_stores"]` (`Cargo.toml:25`).
- `ahash` pulls dep:ahash + hashbrown/default and is enabled for everyone, including users who
  only need #[once] or an UnboundCache.

## Desired work

- Keep proc_macro and time_stores in default, move ahash out so users opt in with
  features = ["ahash"].

## Notes

- Genuinely debatable for a cache crate where hashing is hot; decide consciously.
- Migration: add "ahash" to features to keep old behavior; document the rationale either way.
