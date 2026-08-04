//! `KeyedCache` (the `sync_writes = "by_key"` support type) must live under
//! `cached::__private`, not at the crate root.
//!
//! It is `#[doc(hidden)]`, but `#[doc(hidden)]` is a rustdoc concept, not a name-resolution
//! one: while it was a root-level `pub` item, rustc offered it as the nearest-match "did you
//! mean" suggestion for unrelated mistyped imports such as `use cached::TimedCache;` (a store
//! name removed in 1.0). Moving it into a module keeps it reachable for generated code while
//! taking it out of `cached::*` root resolution.

// ── The `__private` path resolves and is usable ──────────────────────────────

#[test]
fn keyed_cache_resolves_under_private_module() {
    use cached::__private::KeyedCache;
    use std::sync::Arc;

    let buckets: Vec<Arc<parking_lot::RwLock<()>>> = (0..4)
        .map(|_| Arc::new(parking_lot::RwLock::new(())))
        .collect();
    let keyed: KeyedCache<parking_lot::RwLock<u32>, parking_lot::RwLock<()>> =
        KeyedCache::new(parking_lot::RwLock::new(7), buckets);

    // The only stable surface: `Deref` to the inner cache lock.
    assert_eq!(*keyed.read(), 7);
    *keyed.write() = 9;
    assert_eq!(*keyed.read(), 9);

    // Bucket selection is stable for a given key within one static.
    let a = keyed.bucket_for(&"key");
    let b = keyed.bucket_for(&"key");
    assert!(
        Arc::ptr_eq(a, b),
        "the same key must map to the same bucket lock"
    );
}

/// Same, spelled as a fully-qualified path rather than a `use`, so the module itself (not just
/// the item) is proven public.
#[test]
fn private_module_is_reachable_as_a_path() {
    let keyed =
        cached::__private::KeyedCache::<parking_lot::Mutex<u8>, parking_lot::Mutex<()>>::new(
            parking_lot::Mutex::new(1),
            vec![std::sync::Arc::new(parking_lot::Mutex::new(()))],
        );
    assert_eq!(*keyed.lock(), 1);
}

// ── The crate root path no longer resolves ───────────────────────────────────
//
// The *negative* half of this certification cannot live in an integration test: a failing
// `use cached::KeyedCache;` fails the whole test crate's build rather than a single test, and
// doc comments in `tests/*.rs` are never collected as doctests. It is asserted instead by the
// `compile_fail` doctests on the `cached::__private` module in `src/lib.rs`, which cover
// `use cached::KeyedCache;` and `use cached::*;` and run under `cargo test --doc`.
//
// The one thing an integration test can still show is that root and `__private` are distinct
// namespaces: a local `KeyedCache` beside a crate-root glob binds to the local item.

#[test]
fn root_glob_and_private_path_are_distinct_namespaces() {
    #[allow(unused_imports)]
    use cached::*;

    // A distinct local type with the same name; the glob above contributes nothing for it.
    struct KeyedCache(u8);

    let local = KeyedCache(3);
    assert_eq!(local.0, 3);

    // The real type is still reachable, but only through `__private`.
    let keyed =
        cached::__private::KeyedCache::<parking_lot::Mutex<u8>, parking_lot::Mutex<()>>::new(
            parking_lot::Mutex::new(local.0),
            vec![std::sync::Arc::new(parking_lot::Mutex::new(()))],
        );
    assert_eq!(*keyed.lock(), 3);
}
