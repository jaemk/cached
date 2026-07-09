# 0019 - Drop ahash from default features

Status: Decided: keep in defaults (DEC-3=A)

## Current state

- `default = ["proc_macro", "ahash", "time_stores"]` (`Cargo.toml:22`).
- `ahash` pulls dep:ahash + hashbrown/default and is enabled for everyone, including users who
  only need #[once] or an UnboundCache.

## Desired work

- Keep proc_macro and time_stores in default, move ahash out so users opt in with
  features = ["ahash"].

## Notes

- Genuinely debatable for a cache crate where hashing is hot; decide consciously.
- Migration: add "ahash" to features to keep old behavior; document the rationale either way.

## Decision

DEC-3=A: keep `ahash` in default features. The hash-flood concern is already addressed via
runtime-generated random seeds on non-wasm platforms (`Cargo.toml` ahash config block,
`Cargo.toml:97-107`). Hashing is hot in a cache crate; dropping ahash from defaults would be
a silent performance regression for users relying on them. No code change.
