# 0003 - Redis millisecond TTL

Status: Implemented

## Current state

- The Redis store rounds any sub-second TTL up to one whole second and writes with `SETEX` /
  `EXPIRE` (`src/stores/redis.rs:78`). Builders accept `.ttl_millis(...)` and `.ttl(Duration)`
  but the millisecond resolution is discarded at write time, so `ttl_millis(250)` becomes a
  1000ms Redis TTL.
- `ttl_millis` is advertised as a feature in the macro and store docs, but it is inaccurate
  below one second on a Redis-backed store.

## Desired work

- Convert all TTLs to milliseconds before issuing the Redis command and use the millisecond
  commands `PSETEX` / `PEXPIRE` (available since Redis 2.6) so the stored TTL matches the
  requested `Duration` to the millisecond.
- Drop the round-up-to-1s path. Update the internal `ttl_seconds`/`ttl_seconds_i64` helpers and
  their tests (`src/stores/redis.rs:188`) to millisecond variants. Keep the saturating clamp at
  `i64::MAX` in milliseconds.
- Update docs that describe whole-second Redis TTL granularity.

## Notes

- Behavior of existing whole-second TTLs is unchanged (`PSETEX` with `secs * 1000`). Only
  sub-second TTL users see corrected timing. Targets only Redis older than 2.6 (circa 2012),
  which is negligible.
