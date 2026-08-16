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

## Reversal (pre-3.0.0)

DEC-6=A is reverted. The premise ("enable serde support independent of choosing redis or
redb") did not hold: no public item is gated on the feature. `SerializeCached` and
`SerializeCachedAsync` are ungated traits with no exposed codec, and `grep 'feature =
"serde"' src/` matched nothing, so `features = ["serde"]` added `serde` + `rmp-serde` to a
consumer's dependency graph and no API. `redis_store` and `redb_store` now list
`dep:serde` / `dep:rmp-serde` directly and the public feature is gone. Removing a public
feature is breaking, so it lands in 3.0.0 rather than a 3.x minor.

Re-adding a `serde` feature is only worthwhile alongside actual gated API (exposed codec
helpers a custom `SerializeCached` store would need); that would be additive at any time.
