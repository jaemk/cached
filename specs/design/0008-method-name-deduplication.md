# 0008 - Collapse dual method names

Status: Implemented

## Current state

- Every operation on `Cached` exists under two non-deprecated names: a short alias and a
  `cache_`-prefixed form. `cache_get`/`get`, `cache_set`/`set`, `cache_remove`/`remove`,
  `cache_remove_entry`/`remove_entry`, `cache_clear`/`clear`, `cache_size`/`len`,
  `cache_delete`/`delete`, `cache_try_set`/`try_set`, the four `*get_or_set_with*` pairs,
  `cache_hits`/`hits`, `cache_misses`/`misses` (`src/lib.rs:913` onward; aliases from
  `src/lib.rs:1142`). `ConcurrentCached` repeats the pattern (`src/lib.rs:1812`).
- `Cached` is roughly 40 public methods, over half pure delegations. This is the trait
  implementors read and the prelude pulls in.

## Desired work

- Keep the `cache_`-prefixed methods as the required core trait surface and move the short names
  (`get`/`set`/`remove`/`clear`/`len`/...) to a blanket extension trait (`CachedExt`, and a
  concurrent counterpart) with default impls delegating to the core methods.
- This shrinks the implementor surface (custom stores implement only the core methods) without
  removing any caller-facing name. Re-export the extension traits from the prelude.

## Notes

- Chosen over deleting one spelling outright (lowest-regret: no caller-facing name disappears).
- Migration: low. Existing call sites keep compiling once the extension trait is in scope (it is
  in the prelude). Custom `impl Cached` blocks shrink.
