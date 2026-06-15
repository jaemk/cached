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
}
