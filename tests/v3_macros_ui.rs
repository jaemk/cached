/*!
Trybuild compile-fail goldens for the 3.0 macro changes.

These cover the new attribute validations:
- `ttl` + `ttl_millis`, `ttl` + `ttl_secs`, and `ttl_secs` + `ttl_millis`
  mutual exclusion (the 3-way exclusion) on all three macros.
- the old `ttl = <integer>` whole-seconds form rejected with the migration
  message pointing at `ttl_secs`/`ttl_millis`, on all three macros.
- `ttl_millis = 0` rejection (#149) on all three macros.
- `expires` + `ttl_millis` mutual exclusion (#149) on all three macros.
- an unparseable `ttl` Duration expression on all three macros.
- an unparseable `force_refresh` expression on all three macros (#146).
- a generic (type-param) function and a generic `in_impl` method without
  `key`/`convert` (the generic rejection, #80) on both `#[cached]` and
  `#[concurrent_cached]`.
- a const-generic function without `key`/`convert` on both `#[cached]` and
  `#[concurrent_cached]` (const params have the same static-naming problem as
  type params and are now rejected with the same message).
- `in_impl = true` on an associated function without a `self` receiver (the
  `in_impl`-requires-self rejection) on all three macros.
- a `create` block combined with `ttl_millis` (the create-conflict rejection,
  #149) on both `#[cached]` and `#[concurrent_cached]`.

All of these fire during macro expansion before any feature-gated store type is
emitted, so `proc_macro` alone is sufficient (no `time_stores` needed).
*/

#![cfg(feature = "proc_macro")]

#[test]
fn compile_fail_v3_macros() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/cached_ttl_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/cached_ttl_millis_zero.rs");
    t.compile_fail("tests/ui/once_ttl_millis_zero.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_millis_zero.rs");
    t.compile_fail("tests/ui/once_ttl_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/cached_ttl_ttl_secs_exclusive.rs");
    t.compile_fail("tests/ui/once_ttl_ttl_secs_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_ttl_secs_exclusive.rs");
    t.compile_fail("tests/ui/cached_ttl_secs_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/once_ttl_secs_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_secs_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/cached_ttl_unparseable_duration.rs");
    t.compile_fail("tests/ui/once_ttl_unparseable_duration.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_unparseable_duration.rs");
    t.compile_fail("tests/ui/cached_ttl_integer_migration.rs");
    t.compile_fail("tests/ui/once_ttl_integer_migration.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_integer_migration.rs");
    t.compile_fail("tests/ui/cached_expires_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/once_expires_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_and_ttl_millis_exclusive.rs");
    t.compile_fail("tests/ui/cached_force_refresh_unparseable.rs");
    t.compile_fail("tests/ui/once_force_refresh_unparseable.rs");
    t.compile_fail("tests/ui/concurrent_cached_force_refresh_unparseable.rs");
    t.compile_fail("tests/ui/cached_generic_requires_convert.rs");
    t.compile_fail("tests/ui/cached_const_generic_requires_convert.rs");
    t.compile_fail("tests/ui/cached_in_impl_generic_requires_convert.rs");
    t.compile_fail("tests/ui/concurrent_cached_generic_requires_convert.rs");
    t.compile_fail("tests/ui/concurrent_cached_const_generic_requires_convert.rs");
    t.compile_fail("tests/ui/concurrent_cached_in_impl_generic_requires_convert.rs");
    t.compile_fail("tests/ui/cached_ttl_millis_create_conflict.rs");
    t.compile_fail("tests/ui/cached_refresh_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_millis_create_conflict.rs");
    t.compile_fail("tests/ui/cached_in_impl_requires_self.rs");
    t.compile_fail("tests/ui/once_in_impl_requires_self.rs");
    t.compile_fail("tests/ui/concurrent_cached_in_impl_requires_self.rs");
    // Item 2: `name` must be a valid Rust identifier
    t.compile_fail("tests/ui/cached_name_invalid_ident.rs");
    t.compile_fail("tests/ui/once_name_invalid_ident.rs");
    t.compile_fail("tests/ui/concurrent_cached_name_invalid_ident.rs");
    // Item 2 edge cases: leading digit and reserved keyword are also rejected
    // (same spanned message) and must not reach `Ident::new` (which panics on
    // a keyword).
    t.compile_fail("tests/ui/cached_name_leading_digit.rs");
    t.compile_fail("tests/ui/cached_name_keyword.rs");
    // Item 11: `ShardHasher: Clone` supertrait - a non-Clone custom hasher is rejected.
    t.compile_fail("tests/ui/sharded_non_clone_shard_hasher.rs");
    // Negative surface for the concurrent trait split: non-TTL sharded stores do not
    // implement `ConcurrentCacheTtl`, so `set_ttl` does not exist on them even under
    // the prelude glob.
    t.compile_fail("tests/ui/sharded_unbound_no_set_ttl.rs");
    // Item 9: `#[cached]`-only attributes rejected on other macros
    t.compile_fail("tests/ui/once_sync_lock_unsupported.rs");
    t.compile_fail("tests/ui/once_unsync_reads_unsupported.rs");
    t.compile_fail("tests/ui/concurrent_cached_sync_writes_buckets_unsupported.rs");
    t.compile_fail("tests/ui/concurrent_cached_sync_lock_unsupported.rs");
    t.compile_fail("tests/ui/concurrent_cached_unsync_reads_unsupported.rs");
    // Item #1: explicit sync_writes = "by_key" combined with result_fallback errors.
    t.compile_fail("tests/ui/cached_result_fallback_sync_writes_by_key.rs");
    // Item #2: malformed unquoted convert block (syntax error) rejected.
    t.compile_fail("tests/ui/cached_convert_malformed_unquoted.rs");
    // Item #2: map_error = 5 (non-closure expression) rejected.
    t.compile_fail("tests/ui/concurrent_cached_map_error_non_closure.rs");
    // G1: generic `#[once]` whose value type names a function type parameter.
    t.compile_fail("tests/ui/once_generic_value_type_rejected.rs");
    // G1: value type names the param *nested* inside another generic (`Vec<T>`) -
    // a genuine whole-ident match that the substring->whole-ident fix must keep
    // catching (not a false-rejection).
    t.compile_fail("tests/ui/once_generic_value_type_nested_rejected.rs");
    // G1: value type names a function *const* parameter (`[u8; N]`); the walk
    // descends into the bracket group to find `N`.
    t.compile_fail("tests/ui/once_generic_const_value_type_rejected.rs");
    // G2: `name` beginning with `__cached` is reserved on all three macros.
    t.compile_fail("tests/ui/cached_name_reserved_prefix.rs");
    t.compile_fail("tests/ui/once_name_reserved_prefix.rs");
    t.compile_fail("tests/ui/concurrent_cached_name_reserved_prefix.rs");
    // D11: `sync_writes_buckets` is inert on `#[once]` (no `by_key` support).
    t.compile_fail("tests/ui/once_sync_writes_buckets_inert.rs");
}
