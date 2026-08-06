/*!
Independent certification coverage for `companions = false` on `#[cached]` /
`#[once]` / `#[concurrent_cached]`, beyond `tests/companions_optout.rs`.

`companions = false` drops the `{fn}_no_cache` / `{fn}_prime_cache` free functions.
For `#[cached]` without `in_impl`, the origin body that used to live in the
module-scope `{fn}_no_cache` moves into a function-local `fn` nested inside the
cached wrapper instead of disappearing, so `return`/`?` in the user's body stay
bound to the user's own `fn` rather than the wrapper's. This file closes the gaps
the implementor flagged as unexercised by `tests/companions_optout.rs`:

 1. A generic `#[cached(companions = false)]` function with `key` + `convert`
    (the nested origin re-declares the function's own generics inside the
    wrapper's block) -- plus lifetime-only generics, const generics, and `where`
    clauses in isolation.
 2. The `use ::cached::Cached;` the macro injects into the wrapper's block sits in
    an enclosing scope relative to the nested origin body; a user item literally
    named `Cached` at module scope must still resolve correctly from inside the
    nested body.
 3. `companions = false` combined with `async`, `sync_writes = "by_key"`,
    `unsync_reads`, and `result_fallback`, plus `#[cfg]`/`#[allow]` attribute
    forwarding into the nested case.
 4. `#[concurrent_cached(expires = true, result_fallback = true, max_size = N)]`,
    including the `_prime_cache` companion on that path (present by default,
    absent under `companions = false`).
*/

#![cfg(feature = "proc_macro")]

use std::sync::atomic::{AtomicUsize, Ordering};

use cached::macros::{cached, concurrent_cached};

// ── generics: type param + key/convert, lifetime-only, const generic, where ────

mod generics {
    use super::*;

    // Type-parameter generic with `key` + `convert` (required by the macro's own
    // guard once a type param is present) and a `where` clause. The nested origin
    // clones the full signature -- including `<T>` and the bound -- into a
    // function-local `fn`, which is a distinct, independently-generic item from
    // the wrapper (Rust permits a nested item to redeclare a same-named generic
    // parameter; it is not reusing the wrapper's `T`).
    static TYPE_GENERIC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(companions = false, key = "String", convert = r#"{ x.to_string() }"#)]
    fn type_generic_where<T>(x: T) -> String
    where
        T: std::string::ToString + Clone,
    {
        TYPE_GENERIC_CALLS.fetch_add(1, Ordering::SeqCst);
        x.to_string()
    }

    #[test]
    fn type_generic_with_key_convert_and_where_clause_caches_under_no_companions() {
        TYPE_GENERIC_CALLS.store(0, Ordering::SeqCst);

        assert_eq!(type_generic_where(7u32), "7");
        assert_eq!(type_generic_where(7u32), "7");
        assert_eq!(
            TYPE_GENERIC_CALLS.load(Ordering::SeqCst),
            1,
            "second call with the same key must be a cache hit"
        );

        // A different monomorphization shares the same String-keyed store.
        assert_eq!(type_generic_where(8u64), "8");
        assert_eq!(TYPE_GENERIC_CALLS.load(Ordering::SeqCst), 2);
    }

    // Lifetime-only generic: no type or const parameter, so the macro's
    // key/convert guard does not even apply, but the nested origin still has to
    // carry the lifetime parameter through unscathed.
    static LIFETIME_GENERIC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(companions = false, key = "String", convert = r#"{ x.to_string() }"#)]
    // The explicit lifetime is the point under test (a lifetime-only generic
    // function composed with `companions = false`); clippy would otherwise
    // suggest eliding it.
    #[allow(clippy::needless_lifetimes)]
    fn lifetime_only_generic<'a>(x: &'a str) -> String {
        LIFETIME_GENERIC_CALLS.fetch_add(1, Ordering::SeqCst);
        x.to_uppercase()
    }

    #[test]
    fn lifetime_only_generic_caches_under_no_companions() {
        LIFETIME_GENERIC_CALLS.store(0, Ordering::SeqCst);

        assert_eq!(lifetime_only_generic("abc"), "ABC");
        assert_eq!(lifetime_only_generic("abc"), "ABC");
        assert_eq!(LIFETIME_GENERIC_CALLS.load(Ordering::SeqCst), 1);

        assert_eq!(lifetime_only_generic("xyz"), "XYZ");
        assert_eq!(LIFETIME_GENERIC_CALLS.load(Ordering::SeqCst), 2);
    }

    // Const generic: `N` is part of the argument type (`&[i32; N]`), so the
    // compiler infers it at each call site; the nested origin's own re-declared
    // `const N: usize` must still bind correctly when invoked from the wrapper.
    static CONST_GENERIC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(
        companions = false,
        key = "String",
        convert = r#"{ format!("{:?}", arr) }"#
    )]
    fn const_generic_no_companions<const N: usize>(arr: &[i32; N]) -> usize {
        CONST_GENERIC_CALLS.fetch_add(1, Ordering::SeqCst);
        arr.len()
    }

    #[test]
    fn const_generic_caches_under_no_companions() {
        CONST_GENERIC_CALLS.store(0, Ordering::SeqCst);

        let arr = [1i32, 2, 3];
        assert_eq!(const_generic_no_companions(&arr), 3);
        assert_eq!(const_generic_no_companions(&arr), 3);
        assert_eq!(
            CONST_GENERIC_CALLS.load(Ordering::SeqCst),
            1,
            "second call with the same key must be a cache hit"
        );

        let arr2 = [1i32, 2, 3, 4];
        assert_eq!(const_generic_no_companions(&arr2), 4);
        assert_eq!(CONST_GENERIC_CALLS.load(Ordering::SeqCst), 2);
    }
}

// ── the injected `use ::cached::Cached;` must not shadow a user `Cached` item ──

// CONFIRMED DEFECT (reported, not fixed here -- source is out of scope for this
// certification pass): a module-level item literally named `Cached`, referenced
// *unqualified* from inside a `companions = false` function body, does NOT
// compile. The macro injects `use ::cached::Cached;` into the wrapper's block
// (needed for `.cache_get`/`.cache_set`), and that import sits in a scope nearer
// to the nested origin body than the module-level item, so it wins name
// resolution and shadows the user's own `Cached`. Reproduction (fails with
// E0782 "expected a type, found a trait" -- `Cached::marker` resolves to the
// crate's `Cached` trait, which has no `marker` associated function):
//
//   struct Cached { value: u32 }
//   impl Cached { fn marker(value: u32) -> Self { Cached { value } } }
//
//   #[cached(companions = false)]
//   fn body_defines_own_cached_item(x: u32) -> u32 {
//       let local = Cached::marker(x);   // E0782: resolves to ::cached::Cached
//       local.value * 2
//   }
//
// That shadowing has since been fixed: the macro no longer emits any `use` into
// a scope enclosing user tokens, and every store call is written as a fully
// qualified trait path instead. The reproduction above now compiles, and is
// committed as a passing test in `tests/companions_optout.rs`.
//
// The same injected imports also reached the `convert` and `force_refresh`
// expressions on every `#[cached]` expansion, so the exposure was never limited
// to `companions = false`. The tests below remain useful as scope-resolution
// coverage rather than as documentation of a workaround.
mod cached_trait_scope_leak {
    use super::*;

    /// A body that imports its own `Cached` under a narrower, function-local
    /// `use`. The narrowest (innermost) declaration wins, so this resolves to the
    /// function-local import. It now has nothing to compete with, since the macro
    /// injects no imports, but it still pins that generated code cannot disturb
    /// name resolution inside the user's body.
    mod inner_use_shadow {
        use super::*;

        mod other {
            pub struct Cached {
                pub tag: &'static str,
            }
            impl Cached {
                pub fn marker() -> Self {
                    Cached { tag: "other" }
                }
            }
        }

        static INNER_USE_CALLS: AtomicUsize = AtomicUsize::new(0);

        #[cached(companions = false)]
        fn body_imports_its_own_cached(x: u32) -> u32 {
            use self::other::Cached;
            INNER_USE_CALLS.fetch_add(1, Ordering::SeqCst);
            let marker = Cached::marker();
            assert_eq!(marker.tag, "other");
            x + 1
        }

        #[test]
        fn body_local_use_of_cached_shadows_both_outer_definitions() {
            INNER_USE_CALLS.store(0, Ordering::SeqCst);
            assert_eq!(body_imports_its_own_cached(1), 2);
            assert_eq!(body_imports_its_own_cached(1), 2);
            assert_eq!(INNER_USE_CALLS.load(Ordering::SeqCst), 1);
        }
    }
}

// ── companions = false combined with async / sync_writes / unsync_reads / result_fallback ──

mod attribute_combinations {
    use super::*;

    #[cfg(feature = "async")]
    mod async_no_companions {
        use super::*;

        static ASYNC_CALLS: AtomicUsize = AtomicUsize::new(0);

        #[cached(companions = false)]
        async fn async_body_no_companions(x: u32) -> u32 {
            ASYNC_CALLS.fetch_add(1, Ordering::SeqCst);
            x * 2
        }

        #[tokio::test]
        async fn async_companions_false_caches_and_awaits_nested_origin() {
            ASYNC_CALLS.store(0, Ordering::SeqCst);

            assert_eq!(async_body_no_companions(4).await, 8);
            assert_eq!(async_body_no_companions(4).await, 8);
            assert_eq!(
                ASYNC_CALLS.load(Ordering::SeqCst),
                1,
                "second call with the same key must be a cache hit"
            );
        }
    }

    static BY_KEY_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(companions = false, sync_writes = "by_key")]
    fn by_key_no_companions(x: u32) -> u32 {
        BY_KEY_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 3
    }

    #[test]
    fn sync_writes_by_key_composes_with_companions_false() {
        BY_KEY_CALLS.store(0, Ordering::SeqCst);

        assert_eq!(by_key_no_companions(5), 15);
        assert_eq!(by_key_no_companions(5), 15);
        assert_eq!(
            BY_KEY_CALLS.load(Ordering::SeqCst),
            1,
            "second call with the same key must be a cache hit through the by-key lock"
        );
        // A distinct key still reaches the (nested) origin.
        assert_eq!(by_key_no_companions(6), 18);
        assert_eq!(BY_KEY_CALLS.load(Ordering::SeqCst), 2);
    }

    static UNSYNC_READS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(companions = false, unsync_reads = true)]
    fn unsync_reads_no_companions(x: u32) -> u32 {
        UNSYNC_READS_CALLS.fetch_add(1, Ordering::SeqCst);
        x * 4
    }

    #[test]
    fn unsync_reads_composes_with_companions_false() {
        UNSYNC_READS_CALLS.store(0, Ordering::SeqCst);

        assert_eq!(unsync_reads_no_companions(2), 8);
        assert_eq!(unsync_reads_no_companions(2), 8);
        assert_eq!(
            UNSYNC_READS_CALLS.load(Ordering::SeqCst),
            1,
            "the shared-lock hit path must still serve the cached value"
        );
    }

    // `result_fallback`: on `Err`, return the last cached `Ok` instead. Mirrors the
    // `always_failing` pattern in `tests/cached.rs` with `companions = false` added.
    // The `ttl_secs` attribute requires `time_stores`, so this pair is gated to keep
    // the file building under feature sets that enable `proc_macro` alone.
    #[cfg(feature = "time_stores")]
    #[cached(companions = false, ttl_secs = 1, result_fallback = true)]
    fn result_fallback_no_companions() -> Result<String, ()> {
        Err(())
    }

    #[cfg(feature = "time_stores")]
    #[test]
    fn result_fallback_composes_with_companions_false() {
        use cached::Cached;

        assert!(result_fallback_no_companions().is_err());

        // Prime a fresh successful value by writing straight to the generated
        // static -- `companions = false` still emits the module-scope cache
        // static, only the free-function companions are suppressed.
        RESULT_FALLBACK_NO_COMPANIONS
            .write()
            .cache_set((), "ok".to_string());
        assert_eq!(
            result_fallback_no_companions(),
            Ok("ok".to_string()),
            "a manually-primed entry must be served as a hit"
        );

        std::thread::sleep(std::time::Duration::from_millis(1_500));

        // Past the TTL: the body reruns, fails, and `result_fallback` substitutes
        // the last-known-good `Ok` instead of surfacing the fresh `Err`.
        assert_eq!(
            result_fallback_no_companions(),
            Ok("ok".to_string()),
            "result_fallback must substitute the last Ok value after expiry, even \
             with companions = false"
        );
    }

    // ── #[cfg] / #[allow] attribute forwarding into the nested origin ──────────

    // `#[cfg(feature = "proc_macro")]` between `#[cached]` and `fn` must reach both
    // the module-scope cache static and the nested origin consistently: this whole
    // file is itself gated on `proc_macro`, so a forwarding mismatch here (one item
    // gated, the sibling not) would produce a hard "cannot find" compile error
    // rather than merely a warning, which is what actually exercises the shape.
    static CFG_FORWARD_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(companions = false)]
    #[cfg(feature = "proc_macro")]
    fn cfg_forwarded_no_companions(x: u32) -> u32 {
        CFG_FORWARD_CALLS.fetch_add(1, Ordering::SeqCst);
        x + 10
    }

    #[test]
    fn cfg_attribute_forwards_consistently_to_the_nested_origin() {
        CFG_FORWARD_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(cfg_forwarded_no_companions(1), 11);
        assert_eq!(cfg_forwarded_no_companions(1), 11);
        assert_eq!(CFG_FORWARD_CALLS.load(Ordering::SeqCst), 1);
    }

    // `#[allow(dead_code)]` between `#[cached]` and `fn` must reach the nested
    // origin's own body: the enclosing `#[deny(dead_code)]` on this module turns
    // an unforwarded allow into a hard compile error rather than a mere warning,
    // so (unlike a bare "it compiles" check) this genuinely fails if the macro
    // stops forwarding non-doc attributes to the nested origin.
    #[deny(dead_code)]
    mod allow_forwarding {
        use super::*;

        static ALLOW_FORWARD_CALLS: AtomicUsize = AtomicUsize::new(0);

        #[cached(companions = false)]
        #[allow(dead_code)]
        fn allow_forwarded_no_companions(x: u32) -> u32 {
            // Never called; without the forwarded `#[allow(dead_code)]` landing on
            // the nested origin item, `#[deny(dead_code)]` above turns this into a
            // compile error instead of a warning.
            fn unused_helper() -> u32 {
                0
            }
            ALLOW_FORWARD_CALLS.fetch_add(1, Ordering::SeqCst);
            x + 20
        }

        #[test]
        fn allow_attribute_forwards_into_the_nested_origin_body() {
            ALLOW_FORWARD_CALLS.store(0, Ordering::SeqCst);
            assert_eq!(allow_forwarded_no_companions(1), 21);
            assert_eq!(ALLOW_FORWARD_CALLS.load(Ordering::SeqCst), 1);
        }
    }
}

// ── #[concurrent_cached(expires = true, result_fallback = true, max_size = N)] ─

mod concurrent_expires_result_fallback_max_size {
    use super::*;
    use cached::Expires;

    #[derive(Clone, Debug, PartialEq)]
    struct ConcurrentExpiringVal {
        val: u32,
        expired: bool,
    }
    impl Expires for ConcurrentExpiringVal {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    // Default companions (true): the `_prime_cache` companion must exist and work
    // on this exact combination (`expires` + `result_fallback` + `max_size`
    // together select `ShardedExpiringLruCache` with the result_fallback codegen
    // path folded in) -- untested elsewhere in combination.
    #[concurrent_cached(
        expires = true,
        result_fallback = true,
        max_size = 4,
        key = "u32",
        convert = "{ k }"
    )]
    fn concurrent_expiring_fallback_with_prime(
        k: u32,
        expired: bool,
        err: bool,
    ) -> Result<ConcurrentExpiringVal, String> {
        if err {
            Err("boom".to_string())
        } else {
            Ok(ConcurrentExpiringVal { val: k, expired })
        }
    }

    #[test]
    fn prime_cache_companion_works_with_expires_result_fallback_and_max_size() {
        // Prime key=1 with a non-expired value via the companion.
        concurrent_expiring_fallback_with_prime_prime_cache(1, false, false).unwrap();
        assert_eq!(
            concurrent_expiring_fallback_with_prime(1, false, false)
                .unwrap()
                .val,
            1
        );

        // Re-prime as expired, then force an error: result_fallback must serve the
        // stale primed value instead of surfacing the fresh Err.
        concurrent_expiring_fallback_with_prime_prime_cache(1, true, false).unwrap();
        let r = concurrent_expiring_fallback_with_prime(1, false, true).unwrap();
        assert_eq!(r.val, 1);
        assert!(r.expired);
    }

    // `companions = false`: the `_prime_cache` companion must be entirely absent on
    // this same combination, while ordinary caching and the result_fallback path
    // still work by calling the cached function directly.
    static NO_COMPANIONS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(
        expires = true,
        result_fallback = true,
        max_size = 4,
        companions = false,
        key = "u32",
        convert = "{ k }"
    )]
    fn concurrent_expiring_fallback_no_companions(
        k: u32,
        expired: bool,
        err: bool,
    ) -> Result<ConcurrentExpiringVal, String> {
        NO_COMPANIONS_CALLS.fetch_add(1, Ordering::SeqCst);
        if err {
            Err("boom".to_string())
        } else {
            Ok(ConcurrentExpiringVal { val: k, expired })
        }
    }

    #[test]
    fn companions_false_still_caches_and_falls_back_without_a_prime_companion() {
        NO_COMPANIONS_CALLS.store(0, Ordering::SeqCst);

        let first = concurrent_expiring_fallback_no_companions(2, false, false).unwrap();
        assert_eq!(first.val, 2);
        let second = concurrent_expiring_fallback_no_companions(2, false, false).unwrap();
        assert_eq!(second.val, 2);
        assert_eq!(
            NO_COMPANIONS_CALLS.load(Ordering::SeqCst),
            1,
            "second call with the same key must be a cache hit"
        );

        // Now force the entry expired-and-erroring: result_fallback must still
        // substitute the stale (fresh-but-marked-expired) Ok value.
        let stale = concurrent_expiring_fallback_no_companions(2, true, true);
        assert!(
            stale.is_ok(),
            "result_fallback must substitute the stale Ok instead of surfacing Err"
        );
    }

    // Absence of `_prime_cache` under `companions = false` is already pinned
    // generically (independent of attribute combination) by
    // `tests/ui/concurrent_cached_companions_false_prime_absent.rs` via
    // `tests/companions_optout.rs`. This module cannot add a further trybuild
    // fixture (only the two `tests/*.rs` coverage files are in scope here), so it
    // sticks to proving the *positive* behavior on this exact attribute
    // combination: ordinary caching and result_fallback both still work by
    // calling the cached function directly with no prime companion involved,
    // which is exercised by the test above.
}
