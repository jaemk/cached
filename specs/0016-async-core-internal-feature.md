# 0016 - Make async_core internal

Status: Needs research

## Current state

- `async_core` is a public, empty marker feature that gates the runtime-agnostic async traits;
  `async` adds async-lock and blocking on top (`Cargo.toml:28`).
- There is no clear standalone use for enabling `async_core` alone.

## Desired work

- Make `async_core` internal (rename to `_async_core` or fold its gating into `async`) so the
  public surface has a single `async` knob.

## Notes

- Verify no example or downstream relies on enabling `async_core` alone before hiding it.
- Migration: low (likely no users).
