# 0004 - Redact `connection_string()` getter

Status: Implemented

## Current state

- The internal `ConnectionString` wrapper redacts credentials in `Debug`/`Display`
  (`src/stores/redis.rs:319`).
- The public `connection_string()` getter on `RedisCache` and `AsyncRedisCache` returns the raw
  URL including any password (`src/stores/redis.rs:682`, `src/stores/redis.rs:1345`), with only
  a doc-comment warning. This re-exposes exactly what the wrapper hides, and leaks credentials
  into logs via `println!("{}", cache.connection_string())`.

## Desired work

- Remove the credential-bearing getter, or change it to return the redacted form (or a
  `&ConnectionString` whose `Display` is safe).
- If a raw accessor is still wanted, gate it behind an explicit name such as
  `connection_string_unredacted()` so the footgun is obvious at the call site.

## Notes

- Migration: low. Few callers need the raw URL back out of the cache; those rename to the
  explicit accessor.
