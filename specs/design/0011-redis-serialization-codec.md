# 0011 - Redis serialization codec

Status: MessagePack switch implemented; pluggable codec needs research

## Current state

- Redis serializes values with rmp-serde (MessagePack) as a versioned `value`/`version`
  envelope stored in bytes (`src/stores/redis.rs:1390`), matching redb; pre-3.0 serde_json
  String entries are still readable via the exact-version JSON gate (see Notes).
- redb uses rmp-serde (MessagePack) on bytes (`src/stores/redb.rs:866`).
- The codec is a private per-store detail; the error enums bake in the concrete serde error
  type.

## Desired work

- Done in 3.0: the Redis store switched from serde_json-as-String to MessagePack (rmp-serde),
  matching redb, storing bytes. This changed the wire format and the error types those variants
  carry (see 0005).
- Needs research: a `Codec` abstraction wired into the builders (builder-set, not a generic
  type param, to avoid signature leakage) defaulting to the per-store choice, so users can pick
  bincode/cbor/json.

## Notes

- The MessagePack switch is a wire-format change; existing Redis entries written by cached 2.x
  are read transparently via the exact-version JSON gate in `deserialize_cached_redis_value`:
  the deserializer tries MessagePack first, then falls back to the pre-3.0 JSON format only if
  the bytes parse as JSON **and** carry `"version": 1` (exact-value check, not merely presence).
  Entries that match neither path return a deserialization error; nothing is silently recomputed.
  See `tests/v3_redis_backward_read.rs` (test `redis_backward_read_legacy_json_entry`) for
  end-to-end coverage.
- Land the Redis error-enum edits together with 0005.
- The pluggable-codec design is unresolved and tracked for later.
