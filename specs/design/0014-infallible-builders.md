# 0014 - Infallible builders return the cache directly

Status: Declined

## Current state

- `UnboundCacheBuilder::build` and `ExpiringCacheBuilder::build` can never fail but return
  `Result<_, BuildError>` and call `.expect("infallible")` internally
  (`src/stores/unbound.rs:120`, `src/stores/expiring.rs:159`).
- The fallible stores' `new(max_size)` constructors panic on a zero/oversized value
  (`src/stores/lru.rs:186`), so the terse constructor panics while the verbose builder is the
  safe one.

## Decision

Declined. `build()` returns `Result<_, BuildError>` uniformly across every store, including the
genuinely-infallible ones. The internal `.expect("infallible")` calls stay.

## Rationale

A single `build() -> Result<_, BuildError>` shape is one thing to learn across the whole builder
surface, rather than two shapes a caller has to know apart per store (which one returns the cache
directly, which one returns a `Result`). It also leaves room to add validation to any builder
later, including one that is infallible today, without that becoming a breaking signature change:
the `Result` is already there to carry a new error variant.

## Notes

- The considered alternative (`build(self) -> UnboundCache<K, V>` for the genuinely-infallible
  builders, dropping the `Result`) is not adopted. Migration would have been mechanical (drop
  `?`/`.unwrap()` at call sites) but is unnecessary since the direction is declined.
- The fallible stores' `new(max_size)` constructors panicking on a zero/oversized value while the
  builder is `Result`-returning stays as documented behavior.
