# 0016 - Make async_core internal

Status: Declined (DEC-2=B)

## Current state

- `async_core` is a public, empty marker feature that gates the runtime-agnostic async traits;
  `async` adds async-lock on top (`Cargo.toml:25-26`).
- There is no clear standalone use for enabling `async_core` alone.

## Desired work

- Make `async_core` internal (rename to `_async_core` or fold its gating into `async`) so the
  public surface has a single `async` knob.

## Notes

- Verify no example or downstream relies on enabling `async_core` alone before hiding it.
- Migration: low (likely no users).

## Decision

DEC-2=B: the collapse/removal proposal is declined. `async_core` is kept as a public,
documented feature ("async trait definitions without the async-lock dependency") for callers
who want the async trait surface without pulling `async-lock`.

5.4 refresh: `blocking` was moved from the `async` feature to `redb_store`; Redis-only and
in-memory async builds no longer pay for it (`Cargo.toml:63-68`).
