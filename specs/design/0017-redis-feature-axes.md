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
  deliberately excluded from the gate (`src/lib.rs:619-645`, `src/stores/mod.rs:340-362`).
- TLS remains fused with the runtime: `redis_smol_native_tls`, `redis_smol_rustls`,
  `redis_tokio_native_tls`, `redis_tokio_rustls` each encode a runtime+TLS combination rather
  than composable axes.

## Desired work

- Make the axes orthogonal: keep redis_tokio/redis_smol as runtime selectors and replace the
  four fused TLS combos with backend-only redis_native_tls/redis_rustls, so a user composes
  "tokio + rustls".
- At minimum, introduce one internal aggregator feature so the 8-way `any()` lists collapse.

## Notes

- Cargo features are additive; an orthogonal TLS feature with no runtime needs a compile_error
  guard or is a no-op.
- If Cargo cannot route one TLS feature to two runtimes cleanly, fall back to the internal
  aggregator.
- Migration: 1:1 table in the guide.
