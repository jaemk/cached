/*!
Trybuild compile-fail goldens for the 3.0 proc-macro-crate attribute validations.

Covers:
- I7: `#[cached(refresh = true)]` rejected when no TTL is set.
- I6: the remaining `#[cached]`-only attributes rejected on `#[once]`
  (`result_fallback`, `refresh`, `max_size`, `ty`, `create`, `key`, `convert`).
- 0013: concurrent-store-only attributes (`disk`, `redis`, `map_error`) rejected
  on `#[cached]` and `#[once]` with a friendly message pointing to
  `#[concurrent_cached]`.

All fire during macro expansion before any feature-gated store type is emitted,
so `proc_macro` alone is sufficient (no `time_stores` needed).
*/

#![cfg(feature = "proc_macro")]

#[test]
fn compile_fail_proc_macro_v3() {
    let t = trybuild::TestCases::new();
    // I7: refresh requires a TTL on `#[cached]`.
    t.compile_fail("tests/ui/cached_refresh_requires_ttl.rs");
    // I6: `#[cached]`-only attributes rejected on `#[once]`.
    t.compile_fail("tests/ui/once_result_fallback_rejected.rs");
    t.compile_fail("tests/ui/once_refresh_rejected.rs");
    t.compile_fail("tests/ui/once_max_size_rejected.rs");
    t.compile_fail("tests/ui/once_ty_rejected.rs");
    t.compile_fail("tests/ui/once_create_rejected.rs");
    t.compile_fail("tests/ui/once_key_rejected.rs");
    t.compile_fail("tests/ui/once_convert_rejected.rs");
    // 0013: concurrent-store-only attributes rejected on `#[cached]` and `#[once]`.
    t.compile_fail("tests/ui/cached_disk_concurrent_only.rs");
    t.compile_fail("tests/ui/cached_redis_concurrent_only.rs");
    t.compile_fail("tests/ui/once_disk_concurrent_only.rs");
    t.compile_fail("tests/ui/once_redis_concurrent_only.rs");
    // A custom `ty` on the redis/disk `#[concurrent_cached]` paths without a matching
    // `create` block is rejected up front (it would otherwise build the default store).
    t.compile_fail("tests/ui/concurrent_cached_redis_ty_without_create.rs");
    t.compile_fail("tests/ui/concurrent_cached_disk_ty_without_create.rs");
    // `cache_prefix_block` (redis-only) rejected on the disk path.
    t.compile_fail("tests/ui/concurrent_cached_disk_cache_prefix_block.rs");
}
