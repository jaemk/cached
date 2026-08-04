# 0042 - Macro missing-feature guards for the disk and redis backends

Status: Implemented

## Previous state

`#[concurrent_cached]` emitted a guard macro invocation for two of its feature-gated code paths
and none of the others:

- Async functions expand `cached::__require_async_feature!{}`, which is a no-op with the `async`
  feature on and a `compile_error!` naming `async` with it off (`src/lib.rs:756`).
- TTL store selection expands `cached::__require_time_stores_feature!{}`, likewise naming
  `time_stores` (`src/lib.rs:782`).

The `disk = true` and `redis = true` store selectors had no equivalent, so selecting a backend
whose Cargo feature was not enabled produced raw resolution errors from the generated code:

- `disk = true` without `redb_store`: E0433 / E0425, "cannot find `RedbCache` in `cached`". The
  message names an internal type and no `cached` feature at all, leaving the reader to guess
  which of `redb_store`, `disk`, or something else is missing.
- `redis = true` without a redis feature: worse, because the async guard fired first. A
  `#[concurrent_cached(redis = true)]` on an async function reached the async path, and the
  resulting diagnostic leaked the doc-hidden internal `cached::__set_dispatch_async` with a rustc
  note pointing at `async_core`. That names a real `cached` feature, which makes it credible, and
  it is the WRONG one: the user needs `redis_tokio` or `redis_smol` (both of which imply `async`),
  and enabling `async_core` as instructed does not fix the build.

## The rule

Both backend selectors get the same guard treatment the async and TTL paths already had:
`#[concurrent_cached(disk = true)]` expands a guard that names `redb_store`, and
`#[concurrent_cached(redis = true)]` expands a guard that names the redis runtime features
(`redis_tokio` / `redis_smol` and their TLS variants), each a no-op when the feature is on. The
macro always emits the invocation because a proc macro cannot see the downstream crate's feature
flags; the declarative guard macro in `cached` is `cfg`'d and decides.

Guard ORDER is part of the rule. The redis guard must be emitted so that it is not pre-empted by
the async guard: an async function with `redis = true` and no redis feature must report the missing
redis feature, not the missing `async` feature. Reporting `async` there is not merely less useful,
it points the user at a feature that will not fix the build, since the redis runtime features are
what enable both the redis backend and its async support.

## What a macro guard cannot do

The guard covers a missing `cached` feature. It cannot pre-empt a missing TRANSITIVE dependency
feature, because the `redis` crate compiles before any `cached` macro expands: by the time a guard
could fire, the dependency has already failed. This is the redis capability-vs-runtime dead end in
[cargo-features.md](../cargo-features.md) FEAT-7, where enabling `redis_connection_manager` or
`redis_async_cache` alone fails inside the `redis` crate with
`compile_error!("tokio-comp or smol-comp features required for aio feature")` and no mention of a
`cached` feature. That case is documentation-only by construction; no macro-side guard can improve
it. The guards added here are scoped to what the macro can actually see and pre-empt.

## Observable surface that changes

Compile diagnostics only. No change to accepted attributes, generated code, or runtime behavior
when the required features are enabled.

- `disk = true` without `redb_store`: a `compile_error!` naming `redb_store` replaces the E0433 /
  E0425 pair.
- `redis = true` without a redis feature: a `compile_error!` naming the redis runtime features
  replaces the `__set_dispatch_async` leak and the misleading `async_core` note.
- Trybuild golden files under `tests/ui/` cover both, so a regression in the guards or in their
  relative order shows up as a `.stderr` diff.

## Notes

- See [cargo-features.md](../cargo-features.md) FEAT-8 and
  [macro-concurrent-cached.md](../macro-concurrent-cached.md) for the shipped behavior; FEAT-7 for
  the transitive-feature case that stays documentation-only.
- Related: 0013 (friendly rejection of store attributes on the wrong macro), the same class of
  change: no new capability, only a diagnostic that names the thing the user has to do.
