# 0028 - per-entry expiry model and set_ttl(0) semantics

Status: Implemented

## Current state

Two expiry models coexist in the library:

**Global TTL** (`CacheTtl` / `ConcurrentCacheTtl`): a single duration applies to every entry
inserted into the store. All TTL-bearing stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`,
`ShardedTtlCache`, `ShardedLruTtlCache`, `RedisCache`, `AsyncRedisCache`, `RedbCache`) implement
this model. The global TTL is mutable at runtime via `set_ttl`/`unset_ttl`.

**Per-entry expiry** (`Expires` trait, `ExpiringCache`, `ExpiringLruCache`): each value
determines its own expiry by implementing `Expires::is_expired()`. There is no global TTL knob;
expiry is a property of the value type, not the store. These stores require `V: Expires` and check
`is_expired()` on every lookup. The `#[cached(expires = true)]` macro attribute selects this model
(unbounded `ExpiringCache` without `max_size`, `ExpiringLruCache` with `max_size`).

## Design decisions recorded here

**`set_ttl(Duration::ZERO)` disables expiry rather than rejecting.** On `CacheTtl`/
`ConcurrentCacheTtl`, `set_ttl` accepts a zero duration as a sentinel meaning "do not expire
newly inserted entries". Pre-existing entries are not touched; they retain whatever expiry was
applied at insert time. This matches the pattern used in the stores themselves (a zero TTL means
"never expire" throughout the store internals, e.g. `src/lib.rs:1648-1650`).

**`try_set_ttl` provides the strict path.** `try_set_ttl` returns `Err(SetTtlError::ZeroTtl)` on
a zero argument. Callers that want to catch an accidental zero (e.g. a TTL derived from a
response header that may be missing) use `try_set_ttl`; callers that want to explicitly disable
expiry use `set_ttl(Duration::ZERO)` or `unset_ttl()`.

**`unset_ttl` is a named alias for the zero-TTL path.** It exists for readability at call sites
that are explicitly opting out of expiry. It is equivalent to `set_ttl(Duration::ZERO)`.

**Per-entry and global TTL are mutually exclusive at the type level.** `expires = true` is
rejected by the macro when `ttl`/`ttl_secs`/`ttl_millis` is also set; this is a compile error.
At the store level there is no shared trait covering both families.

**`Expires::expires_at` is optional and advisory.** The `is_expired()` method is the authoritative
liveness check. `expires_at()` defaults to `None` and is provided for observability (logging,
metrics, deadline comparison) only. See `src/stores/expiring_lru.rs:40-58`.

## Notes

- The `Expires` trait is re-exported from the crate root and the prelude.
- `ExpiringCache` is unbounded and only evicts expired entries on lookup of the same key (plus an
  explicit `evict()` sweep); for high-cardinality workloads `ExpiringLruCache` is preferred.
- Example: `examples/expires_per_key.rs`.
