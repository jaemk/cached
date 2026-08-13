# Cargo feature flags

The `cached` crate gates optional stores, backends, and runtimes behind Cargo features. Defaults:
`proc_macro`, `ahash`, `time_stores`.

## FEAT-1

Core: `proc_macro` (the `#[cached]` / `#[once]` / `#[concurrent_cached]` macros), `ahash` (ahash
hasher for internal maps), `time_stores` (`TtlCache`, `LruTtlCache`, `TtlSortedCache` and their
sharded variants).

## FEAT-2

Async: `async_core` (runtime-agnostic async trait definitions without async-lock; kept public
for callers who want the trait surface without the async-lock dependency), `async`
(enables `async_core` and pulls `async-lock`). Making `async_core` internal was declined
([design/0016-async-core-internal-feature.md](design/0016-async-core-internal-feature.md),
DEC-2=B).

## FEAT-3

Redis: `redis_store` (sync), `redis_tokio` / `redis_smol` (async runtimes, imply `redis_store` +
`async`), their `_native_tls` / `_rustls` TLS variants, plus the capability features
`redis_connection_manager` and `redis_async_cache` (RESP3 client-side caching). A capability
feature requires a runtime feature (documented in `Cargo.toml`; the `redis` crate itself fails to
build otherwise). Orthogonal runtime x TLS axes are an open direction
([design/0017-redis-feature-axes.md](design/0017-redis-feature-axes.md)). See
[store-redis.md](store-redis.md).

## FEAT-4

Disk: `redb_store` (disk-backed cache via `redb`; see [store-redb.md](store-redb.md)). The crate
MSRV is **1.92** (raised from 1.89; see [FEAT-6](#feat-6)).

## FEAT-5

`ahash` remains in the default set (DEC-3=A per
[design/0019-ahash-default-feature.md](design/0019-ahash-default-feature.md)). There is no
public `serde` feature: DEC-6=A was reverted before 3.0.0 (see
[design/0026-serde-feature.md](design/0026-serde-feature.md)) because no public item is gated
on it. `redis_store` and `redb_store` pull `dep:serde` / `dep:rmp-serde` directly.

## FEAT-6

MSRV is **1.92** (raised from 1.89). The `async_core` feature (and `async`, which enables it)
does not compile before 1.92: the two RPIT default bodies on `CachedGetOrSetAsync`
(`async_cache_get_or_set_with`, `async_cache_try_get_or_set_with`) hit a borrowck limitation that
rustc itself attributes to [rust-lang/rust#100013](https://github.com/rust-lang/rust/issues/100013),
producing 11 "does not live long enough" / "lifetime bound not satisfied" errors. Verified by
bisection: fails on 1.89.0, 1.90.0, 1.91.0; builds on 1.92.0. Non-async feature sets did build on
1.89, but `rust-version` is a single crate-level value and must describe the whole crate honestly,
so the floor moves for every feature set, not just the async ones. See
[trait-get-or-set-async.md](trait-get-or-set-async.md). CI now runs a dedicated MSRV job pinned to
1.92 so this cannot regress silently; previously CI only ever ran the pinned 1.96.0 dev toolchain
(see `AGENTS.md`'s Toolchain & Edition section), which is why nine release candidates shipped a
false 1.89 floor.

## FEAT-7

`redis_connection_manager` and `redis_async_cache` are capability features that are
runtime-agnostic: **enabling either alone is a hard dead end.** It fails to compile with four
errors emitted from inside the `redis` crate (starting with
`compile_error!("tokio-comp or smol-comp features required for aio feature")`), none of which name
a `cached` feature. Each must be paired with a runtime feature (`redis_tokio*` or `redis_smol*`).
This cannot be pre-empted by a `cached`-side `compile_error!` because `redis` compiles first, so
the mitigation is documentation only: the pairing requirement is stated as a prominent warning in
the crate's feature table (`AGENTS.md`'s Key Cargo Features table and the `Cargo.toml` feature
comments), not as a mid-paragraph aside. Folding the capability features into
runtime-specific variants (e.g. `redis_tokio_connection_manager`) was considered and declined for
3.0; see [design/0017-redis-feature-axes.md](design/0017-redis-feature-axes.md). See
[store-redis.md](store-redis.md).

## FEAT-8

`#[concurrent_cached(disk = true)]` and `#[concurrent_cached(redis = true)]` emit clear
missing-feature errors naming `redb_store` and the redis runtime features respectively when the
corresponding Cargo feature isn't enabled. The macro-side implementation is documented in
[macro-concurrent-cached.md](macro-concurrent-cached.md); see the decision record at
`specs/design/0042`.
