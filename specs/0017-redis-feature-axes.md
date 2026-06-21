# 0017 - Orthogonal redis runtime x TLS features

Status: Needs research

## Current state

- Eight redis features encode a runtime x TLS cross-product by hand: redis_smol,
  redis_smol_native_tls, redis_smol_rustls, redis_tokio, redis_tokio_native_tls,
  redis_tokio_rustls, plus redis_async_cache and redis_connection_manager
  (`Cargo.toml:30-46`).
- The AsyncRedisCache export is gated on 8-way `any(...)` cfg lists (`src/lib.rs:595`,
  `src/stores/mod.rs:281`).

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
