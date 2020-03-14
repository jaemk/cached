# Changelog

## [Unreleased]
## Added

## Changed
- Change the type of `hits` and `misses` to be `u64`.
 
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
