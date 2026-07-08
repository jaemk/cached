# 0018 - Escape redis key segments

Status: Needs research

## Current state

- `generate_redis_key` joins namespace:prefix:key without escaping interior colons
  (`src/stores/redis.rs:59`), so namespace="a:b" collides with namespace="a", prefix="b".
- The code documents this and a test asserts the collision.

## Desired work

- Length-prefix or percent-escape the segment joins so distinct (namespace, prefix, key) tuples
  always map to distinct Redis keys.

## Notes

- Wire-format (key layout) change; existing keys are recomputed on miss after upgrade.
- Could escape only when a colon is present to keep keys readable in redis-cli.
- Lower priority since it is already documented.
