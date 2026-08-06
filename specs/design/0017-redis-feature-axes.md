# 0017 - Orthogonal redis runtime x TLS features

Status: Capability axis resolved; TLS orthogonality needs research

## Current state

- Eight redis features have been reorganized into a 6-runtime / 2-capability split
  (`Cargo.toml:42-62`).
  - Runtime features (6): `redis_smol`, `redis_smol_native_tls`, `redis_smol_rustls`,
    `redis_tokio`, `redis_tokio_native_tls`, `redis_tokio_rustls`.
  - Capability features (2): `redis_connection_manager`, `redis_async_cache`; both depend only
    on `redis/aio` and carry no runtime -- they must be paired with a runtime feature to
    connect.
- The capability axis was resolved in commit 62083dd: capability features no longer pull
  `redis_tokio`; the connection manager is now additive (per-cache `.connection_manager(true)`
  opt-in rather than a global type swap); CI feature checks pair each capability with both
  runtimes.
- `AsyncRedisCache` is gated on the 6 runtime features only; the 2 capability features are
  deliberately excluded from the gate (`src/lib.rs:639-665`, `src/stores/mod.rs:340-362`).
- TLS remains fused with the runtime: `redis_smol_native_tls`, `redis_smol_rustls`,
  `redis_tokio_native_tls`, `redis_tokio_rustls` each encode a runtime+TLS combination rather
  than composable axes.

## Desired work

- Make the axes orthogonal: keep redis_tokio/redis_smol as runtime selectors and replace the
  four fused TLS combos with backend-only redis_native_tls/redis_rustls, so a user composes
  "tokio + rustls".
- At minimum, introduce one internal aggregator feature so the 8-way `any()` lists collapse.

## Status: deferred to 4.0

Not landing in 3.0. The four fused `redis_{tokio,smol}_{native_tls,rustls}` feature names freeze
as public API at the 3.0 release: renaming or splitting them into orthogonal runtime/TLS features
after that point is a breaking Cargo-feature change, so the earliest this can land is 4.0. The
capability axis (`redis_connection_manager`, `redis_async_cache`) is unaffected and stays resolved
as described above.

## Notes

- Cargo features are additive; an orthogonal TLS feature with no runtime needs a compile_error
  guard or is a no-op.
- If Cargo cannot route one TLS feature to two runtimes cleanly, fall back to the internal
  aggregator.
- Migration: 1:1 table in the guide.
