# 0022 - cache_set_ref returning previous value

Status: Implemented (DEC-1=A)

## Current state

- `SerializeCached::cache_set_ref` and `SerializeCachedAsync::async_cache_set_ref` return the
  previous value `Result<Option<V>, _>` (`src/lib.rs:2070,2094`).
- For serialize-backed stores that means a read+deserialize of the old entry on every set, but
  the trait's only caller (the #[concurrent_cached] fast path) discards the return.

## Desired work

- Change the borrowed setter to return `Result<(), _>`, or split off an explicit swap method
  that returns the previous value.

## Notes

- Performance: avoids a previous-value decode nobody on the fast path consumes (extra round trip
  on Redis, extra table read on redb).
- Migration: custom impls drop the previous-value computation; macro users unaffected.

## Decision

DEC-1=A: `cache_set_ref` and `async_cache_set_ref` now return `Result<(), Self::Error>`.
The redis impl no longer round-trips a GET on the write path; the redb impl drops the prior
table read. Callers that want the previous value must call `cache_get` explicitly (see
generated `__set_dispatch`/`__set_dispatch_async` for the updated call sites).
Migration: custom `SerializeCached` impls must change the return type to `Result<(), ...>`
and return `Ok(())`.
