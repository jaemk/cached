# 0022 - cache_set_ref returning previous value

Status: Needs research

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
