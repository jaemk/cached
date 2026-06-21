# 0024 - Rename or namespace generated companion fns

Status: Needs research

## Current state

- All three macros emit free functions `{fn}_no_cache` and `{fn}_prime_cache` into the parent
  module (`cached_proc_macro/src/cached.rs:824,1126`; `once.rs:565,769`), which can collide with
  user functions.
- `_prime_cache` is tagged `#[allow(dead_code)]`, an admission it is often unused.
- No way to suppress generation.

## Desired work

- Adopt one naming scheme across all three macros (e.g. `{fn}_uncached`/`{fn}_prime`, or a
  generated `{fn}_cache` module namespacing both).
- Add a switch (e.g. `companions = false`) to suppress generation.

## Notes

- A module namespace is the cleaner end state but a bigger break than a rename.
- Migration: rename call sites. Pick one scheme and apply identically to
  #[cached]/#[once]/#[concurrent_cached].
