# Changelog

## [Unreleased]
## Added
- Add a TimedSizedCache combining and LRU and a timed/ttl cache
## Changed
## Removed

## [0.20.0]
## Added
- Add new CachedAsync trait. Only present with async feature. Adds two async function in the entry API style of HashMap
## Changed
## Removed

## [0.19.0] / [0.4.0]
## Added
## Changed
- Add type hint `_result!` macros
- remove unnecessary transmute in cache reset
- remove unnecessary clones in proc macro
## Removed

## [0.18.0] / [0.3.0]
## Added
## Changed
- use `async-mutex` instead of full `async-std`
## Removed

## [0.17.0]
## Added
## Changed
- Store inner values when `result=true` or `option=true`. The `Error` type in the
`Result` now no longer needs to implement `Clone`.
## Removed

## [0.16.0]
## Added
- add `cache_set_lifespan` to change the cache lifespace, old value returned.
## Changed
## Removed

## [0.15.1]
## Added
## Changed
- fix proc macro when result=true, regression from changing `cache_set` to return the previous value
## Removed

## [0.15.0]
## Added
- add `Cached` implementation for std `HashMap`
## Changed
- trait `Cached` has a new method `cache_get_or_set_with`
- `cache_set` now returns the previous value if any
## Removed

## [0.14.0]
## Added
- add Clone, Debug trait derives on pub types

## Changed

## Removed

## [0.13.1]
## Added

## Changed
- fix proc macro documentation

## Removed

## [0.13.0]
## Added
- proc macro version
- async support when using the new proc macro version

## Changed

## Removed

## [0.12.0]
## Added
- Add `cache_get_mut` to `Cached` trait, to allow mutable access for values in the cache.
- Change the type of `hits` and `misses` to be `u64`.

## Changed

## Removed

## [0.11.0]
## Added
- Add `value_order` method to SizedCache, similar to `key_order`

## Changed

## Removed

## [0.10.0]
## Added
- add `cache_reset` trait method for resetting cache collections to
  their initial state

## Changed
- Update `once_cell` to 1.x

## Removed

## [0.9.0]
## Added

## Changed
- Replace SizedCache implementation to avoid O(n) lookup on cache-get
- Update to Rust-2018 edition
- cargo fmt everything

## Removed


## [0.8.1]
## Added

## Changed
- Replace inner cache when "clearing" unbounded cache

## Removed


## [0.8.0]
## Added

## Changed
- Switch to `once_cell`. Library users no longer need to import `lazy_static`

## Removed

## [0.7.0]
## Added
- Add `cache_clear` and `cache_result` to `Cached` trait
  - Allows for defeating cache entries if desired

## Changed

## Removed

## [0.6.2]
## Added

## Changed
- Update documentation
  - Note the in-memory nature of cache stores
  - Note the behavior of memoized functions under concurrent access

## Removed

## [0.6.1]
## Added

## Changed
- Fixed duplicate key eviction in `SizedCache::cache_set`. This would manifest when
  `cached` functions called with duplicate keys would race set an uncached key,
  or if `SizedCache` was used directly.

## Removed

## [0.6.0]
## Added
- Add `cached_result` and `cached_key_result` to allow the caching of success for a function that returns `Result`.
- Add `cached_control` macro to allow specifying functionality
  at key points of the macro

## [0.5.0]
## Added
- Add `cached_key` macro to allow defining the caching key

## Changed
- Tweak `cached` macro syntax
- Update readme

## Removed


## [0.4.4]
## Added

## Changed
- Update trait docs

## Removed


## [0.4.3]
## Added

## Changed
- Update readme
- Update examples
- Update crate documentation and examples

## Removed
