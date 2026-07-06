/*!
Trybuild compile-fail golden for UX-1: `#[cached]`/`#[once]`/`#[concurrent_cached]` on
an async function when the `async` feature of `cached` is disabled should emit a clear
`compile_error!` naming the missing feature, rather than the obscure "cannot find
`async_sync` in `cached`" message.

This harness is gated to run only when `proc_macro` is on and `async` is off: with
`async` enabled the guard macro expands to nothing (correct behavior) and these files
would compile successfully, causing trybuild's `compile_fail` to report a false failure.
*/

#![cfg(all(feature = "proc_macro", not(feature = "async")))]

#[test]
fn compile_fail_async_without_async_feature() {
    let t = trybuild::TestCases::new();
    // All three proc macros emit `cached::__require_async_feature!{}` on an async
    // fn; without the `async` feature that expands to a `compile_error!` naming the
    // missing feature. Cover each macro so the guard cannot regress on one path
    // while the others stay green.
    t.compile_fail("tests/ui/cached_async_requires_async_feature.rs");
    t.compile_fail("tests/ui/once_async_requires_async_feature.rs");
    t.compile_fail("tests/ui/concurrent_cached_async_requires_async_feature.rs");
}
