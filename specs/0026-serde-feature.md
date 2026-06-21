# 0026 - Explicit serde feature for custom serialize stores

Status: Needs research

## Current state

- serde/serde_json are pulled only via redis_store; rmp-serde only via redb_store
  (`Cargo.toml:30,46`).
- There is no top-level serde feature, so a custom serialize-backed store author (using the
  SerializeCached trait) cannot enable serde support independent of choosing redis or redb.

## Desired work

- Add an explicit `serde` feature that the store features depend on, so serde is enableable on
  its own.

## Notes

- Additive (does not require a major), but pairs with the serialize-store extension point.
- Skip if keeping the feature count down is preferred and the audience is niche.
