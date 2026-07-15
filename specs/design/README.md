# Design records

Tracked design items for `cached`, mostly breaking changes scoped to the 3.0 release. Each
file documents one item: current state in the code, the desired work, and a status.

This is the decision log behind the shipped feature surface. For the feature inventory (what
the crate provides and its implementation status), see [../README.md](../README.md); the rows
there link back to the items below for the reasoning. This directory is a working record, not
user-facing docs. Once an item ships, its substance moves to the changelog and migration guide;
the item stays here for history.

## Status legend

- **Implemented** - landed on the 3.0 branch.
- **Not implemented** - agreed direction, not yet built (or a conscious decision not to build).
- **Needs research** - direction is plausible but unresolved; do not build until scoped.

## Index

| Spec | Item | Status |
|---|---|---|
| [0001](0001-non-sharded-custom-hasher.md) | Custom hasher on non-sharded stores | Implemented |
| [0002](0002-size-iter-evict-semantics.md) | `len`/`size` vs `iter` vs `evict` semantics + docs | Implemented |
| [0003](0003-redis-millisecond-ttl.md) | Redis millisecond TTL (`PSETEX`/`PEXPIRE`) | Implemented |
| [0004](0004-redis-connection-string-redaction.md) | Redact `connection_string()` getter | Implemented |
| [0005](0005-store-error-consistency.md) | redb/redis error naming + struct variants | Implemented |
| [0006](0006-macro-quoted-attributes.md) | Retire quoted-string macro attrs | Not implemented (declined) |
| [0007](0007-unbound-evictions-counter.md) | `ShardedUnboundCache` evictions counter | Not implemented (declined) |
| [0008](0008-method-name-deduplication.md) | Collapse dual method names via extension trait | Implemented |
| [0009](0009-cached-get-shared-receiver.md) | `Cached::get` taking `&self` | Needs research |
| [0010](0010-read-optimized-sharded-lru.md) | Read-optimized sharded LRU variant | Needs research |
| [0011](0011-redis-serialization-codec.md) | Redis -> MessagePack; pluggable codec | MessagePack implemented; codec needs research |
| [0012](0012-concurrent-metrics-trait.md) | Expose sharded metrics through a trait | Implemented |
| [0013](0013-macro-store-attribute-placement.md) | Friendly rejection of store attrs on `#[cached]` | Implemented |
| [0014](0014-infallible-builders.md) | Infallible builders return the cache directly | Needs research |
| [0015](0015-sharded-base-alias-collapse.md) | Collapse `*Base` + alias into a defaulted type param | Needs research |
| [0016](0016-async-core-internal-feature.md) | Make `async_core` internal | Needs research |
| [0017](0017-redis-feature-axes.md) | Orthogonal redis runtime x TLS features | Needs research |
| [0018](0018-redis-key-escaping.md) | Escape redis namespace/prefix/key segments | Needs research |
| [0019](0019-ahash-default-feature.md) | Drop `ahash` from default features | Needs research |
| [0020](0020-argument-error-unification.md) | Unify single-variant argument errors | Needs research |
| [0021](0021-redb-refresh-on-hit-cost.md) | Amortize redb refresh-on-hit write txns | Needs research |
| [0022](0022-serialize-cached-set-ref-return.md) | `cache_set_ref` returning previous value | Implemented |
| [0023](0023-peek-read-trait-merge.md) | Merge `CachedPeek`/`CachedRead`; trait fragmentation | Needs research |
| [0024](0024-generated-companion-naming.md) | Rename/namespace generated companion fns | Needs research |
| [0025](0025-redb-disk-path-introspection.md) | redb resolved-path introspection + temp fallback | Needs research |
| [0026](0026-serde-feature.md) | Explicit `serde` feature for custom serialize stores | Implemented |
| [0027](0027-sync-writes-default-revert.md) | `sync_writes` default flip and revert | Implemented |
| [0028](0028-per-entry-expiry-and-set-ttl-zero.md) | Per-entry expiry model and `set_ttl(0)` semantics | Implemented |
| [0029](0029-self-healing-deserialization-default.md) | Self-healing deserialization default | Implemented |
| [0030](0030-force-refresh-result-fallback-interaction.md) | `force_refresh` and `result_fallback` interaction | Implemented |
| [0031](0031-redis-backward-read-version-gate.md) | Redis backward-read version gate | Implemented |
| [0032](0032-cached-async-to-get-or-set-async-rename.md) | `CachedAsync` renamed to `CachedGetOrSetAsync` | Implemented |
| [0033](0033-redb-revalidate-in-write-txn.md) | redb re-validate-in-write-txn design | Implemented |
| [0034](0034-prime-companion-body-before-lock.md) | Prime companion runs body before lock | Implemented |
| [0035](0035-seeded-per-key-lock-bucket-hasher.md) | Seeded per-key lock-bucket hasher | Implemented |
| [0036](0036-in-impl-static-placement.md) | `in_impl` static placement | Implemented |
