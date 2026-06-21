# 0014 - Infallible builders return the cache directly

Status: Needs research

## Current state

- `UnboundCacheBuilder::build` and `ExpiringCacheBuilder::build` can never fail but return
  `Result<_, BuildError>` and call `.expect("infallible")` internally
  (`src/stores/unbound.rs:120`, `src/stores/expiring.rs:159`).
- The fallible stores' `new(max_size)` constructors panic on a zero/oversized value
  (`src/stores/lru.rs:186`), so the terse constructor panics while the verbose builder is the
  safe one.

## Desired work

- Make genuinely-infallible builders return the cache directly
  (`build(self) -> UnboundCache<K,V>`), drop the Result.
- Keep `build() -> Result` for fallible stores.
- Consider a `try_new()` for the runtime-derived-size case.

## Notes

- Migration is mechanical (drop `?`/`.unwrap()` at call sites).
- Different `build()` return shapes across stores is honest about which can fail. Decide
  deliberately for 3.0.
