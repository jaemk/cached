# 0026 - Explicit serde feature for custom serialize stores

Status: Implemented (DEC-6=A)

## Current state

- serde/serde_json are pulled only via redis_store; rmp-serde only via redb_store
  (`Cargo.toml:27,68`).
- There is no top-level serde feature, so a custom serialize-backed store author (using the
  SerializeCached trait) cannot enable serde support independent of choosing redis or redb.

## Desired work

- Add an explicit `serde` feature that the store features depend on, so serde is enableable on
  its own.

## Notes

- Additive (does not require a major), but pairs with the serialize-store extension point.
- Skip if keeping the feature count down is preferred and the audience is niche.

## Decision

DEC-6=A: standalone `serde = ["dep:serde", "dep:rmp-serde"]` feature added. `redis_store`
and `redb_store` list `serde` instead of the individual deps directly. Custom
`SerializeCached` impl authors can now enable `features = ["serde"]` without pulling in
an IO store. `serde_json` remains redis_store-only (MessagePack is the redb codec; JSON
is not used by redb). Feature is documented in the features table in src/lib.rs docs.
