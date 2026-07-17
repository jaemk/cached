# 0031 - redis backward-read version gate

Status: Implemented

## Current state

`RedisCache` and `AsyncRedisCache` write values as MessagePack (`rmp_serde`) with an envelope:

```rust
struct CachedRedisValue<V> {
    value: V,
    version: Option<u64>,  // always Some(REDIS_VALUE_VERSION)
}
```

`REDIS_VALUE_VERSION` is `Some(1)` (`src/stores/redis.rs:1420`). Every write since 3.0 stamps
this version field.

On read, `deserialize_cached_redis_value` (`src/stores/redis.rs:1468-1493`) tries MessagePack
first. On failure it attempts a legacy JSON fallback: the bytes are parsed as a generic JSON
value, and only if the result carries `"version": 1` (the exact constant value, not merely a
`version` key) is a `serde_json` deserialization attempted. If neither path succeeds, the
original MessagePack error is returned.

## Design decisions recorded here

**MessagePack is tried first.** It is the current format; the JSON path is only reached for
entries written by cached 2.x.

**The version field value is checked, not merely its presence.** `json.get("version") ==
Some(&serde_json::json!(REDIS_VALUE_VERSION))` compares the value to `Some(1)`, not just checks
`is_some()`. A JSON object with `"version": 99` or `"version": null` is not treated as legacy
and returns the original MessagePack error. This prevents an unrelated JSON object (e.g. from a
key collision with a non-cached application) from being silently accepted as a cached value.
`tests/v3_redis_backward_read.rs` pins this behavior, including the rejection of `version: 99`
and `version: null` cases (`src/stores/redis.rs:554-600`).

**The original MessagePack error is preserved.** If neither decode path succeeds, the error
returned to the caller carries the MessagePack `source` and the raw `cached_value: Vec<u8>`.
The JSON error is discarded. Rationale: for entries written since 3.0, a decode failure is a
MessagePack error; the JSON attempt is a silent fallback heuristic, not a primary path.

**The gate is transparent to callers.** From the caller's perspective, `cache_get` returns
`Ok(Some(v))` whether the value was stored as MessagePack or legacy JSON. The self-heal and
strict-mode paths both go through `deserialize_cached_redis_value`, so they also benefit from
the backward-read.

## Notes

- The legacy JSON gate is temporary scaffolding for the 2.x -> 3.0 migration window. Once all
  keys in a deployment have been rewritten by 3.0, the JSON path is never reached. There is no
  plan to remove it before 4.0; it is cheap (MessagePack succeeds on the fast path) and harmless.
- Tests: `tests/v3_redis_backward_read.rs`; `src/stores/redis.rs:664-717` (unit tests for the
  version gate logic).
- Spec 0011 (redis serialization codec) covers the MessagePack migration itself; this spec covers
  only the backward-read version gate.
