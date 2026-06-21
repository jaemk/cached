# 0011 - Redis serialization codec

Status: MessagePack switch implemented; pluggable codec needs research

## Current state

- Redis serializes values with serde_json and stores a UTF-8 String
  (`src/stores/redis.rs:824`).
- redb uses rmp-serde (MessagePack) on bytes (`src/stores/redb.rs:776`).
- The codec is a private per-store detail; the error enums bake in the concrete serde error
  type.

## Desired work

- Target 3.0: switch the Redis store from serde_json-as-String to MessagePack (rmp-serde),
  matching redb, storing bytes. This changes the wire format and the error types those variants
  carry (see 0005).
- Needs research: a `Codec` abstraction wired into the builders (builder-set, not a generic
  type param, to avoid signature leakage) defaulting to the per-store choice, so users can pick
  bincode/cbor/json.

## Notes

- The MessagePack switch is a wire-format change; existing Redis entries are recomputed on miss,
  same tradeoff already accepted for the sled->redb backend change.
- Land the Redis error-enum edits together with 0005.
- The pluggable-codec design is unresolved and tracked for later.
