//! Certification for the `ConcurrentCachePeekAsync` trait (specs/traits-concurrent.md CTRAIT-5,
//! specs/design/0040-peek-is-an-in-memory-concept.md).
//!
//! Asserted here:
//! - the trait exists with the required
//!   `async_cache_peek(&self, &K) -> impl Future<Output = Result<Option<V>, Self::Error>> + Send`
//! - `async_cache_peek` has NO default body: implementing `ConcurrentCacheBase` (even together
//!   with `ConcurrentCachedAsync`) does not satisfy the `ConcurrentCachePeekAsync` bound, so an
//!   implementor is forced to write a genuinely side-effect-free read
//! - the defaulted `async_peek` alias delegates to `async_cache_peek`
//! - the trait is re-exported from `cached::prelude`
//! - a generic bound over the trait is usable from async code
//!
//! The six sharded stores' impls live in `src/stores/**` and are certified elsewhere; this file
//! only exercises the trait definition, via a local store, so it stays independent of them.

#![cfg(feature = "async_core")]

use cached::{ConcurrentCacheBase, ConcurrentCachePeekAsync};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Local store implementing the trait ───────────────────────────────────────

/// A minimal concurrent store. `get`-style reads bump `reads`; peeks must not.
#[derive(Default)]
struct PeekableStore {
    map: Mutex<HashMap<u32, String>>,
    /// Counts side-effectful reads. A conforming `async_cache_peek` never touches it.
    reads: AtomicUsize,
    /// Counts `async_cache_peek` calls, so the `async_peek` alias can be shown to delegate.
    peeks: AtomicUsize,
}

impl PeekableStore {
    fn insert(&self, k: u32, v: &str) {
        self.map.lock().unwrap().insert(k, v.to_string());
    }

    /// A deliberately side-effectful read, standing in for `async_cache_get`.
    fn counted_get(&self, k: &u32) -> Option<String> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.map.lock().unwrap().get(k).cloned()
    }
}

impl ConcurrentCacheBase for PeekableStore {
    type Error = Infallible;
}

impl ConcurrentCachePeekAsync<u32, String> for PeekableStore {
    // Required method: there is no default body to inherit, so the side-effect-free
    // contract is written by the implementor.
    fn async_cache_peek(
        &self,
        k: &u32,
    ) -> impl std::future::Future<Output = Result<Option<String>, Self::Error>> + Send {
        self.peeks.fetch_add(1, Ordering::Relaxed);
        let v = self.map.lock().unwrap().get(k).cloned();
        async move { Ok(v) }
    }
}

/// A store that implements only the base trait — used to prove there is no blanket impl and no
/// default body handing `ConcurrentCachePeekAsync` out for free.
struct BaseOnlyStore;

impl ConcurrentCacheBase for BaseOnlyStore {
    type Error = Infallible;
}

// ── "Does `T` implement the trait?" detector ─────────────────────────────────
//
// The inherent `impl` on `Detect<T>` is only applicable when `T: ConcurrentCachePeekAsync`, and
// inherent items take priority over trait items, so `Detect::<T>::IMPLEMENTED` resolves to
// `true` exactly when the bound holds and falls back to the trait default otherwise.

struct Detect<T>(std::marker::PhantomData<T>);

trait DetectFallback {
    const IMPLEMENTED: bool = false;
}
impl<T> DetectFallback for Detect<T> {}

impl<T: ConcurrentCachePeekAsync<u32, String>> Detect<T> {
    const IMPLEMENTED: bool = true;
}

#[test]
fn peek_async_is_a_separate_trait_with_no_default_body() {
    // Asserted in `const` blocks: these are compile-time facts about trait resolution, so a
    // regression fails the build rather than the test run.

    // An explicit impl satisfies the bound...
    const {
        assert!(
            Detect::<PeekableStore>::IMPLEMENTED,
            "an explicit `ConcurrentCachePeekAsync` impl must satisfy the bound"
        );
    }
    // ...while `ConcurrentCacheBase` alone does not. If `async_cache_peek` were defaulted onto
    // an existing trait (or handed out by a blanket impl) this would be `true`, and the
    // side-effect-free contract would not be enforceable on implementors.
    const {
        assert!(
            !Detect::<BaseOnlyStore>::IMPLEMENTED,
            "`ConcurrentCacheBase` alone must NOT satisfy `ConcurrentCachePeekAsync`"
        );
    }
}

// ── Generic bound over the trait ─────────────────────────────────────────────

/// Generic async helper bounded on the trait: only compiles because the trait exists with the
/// required method and an RPIT `Send` future.
async fn peek_via_trait<S>(s: &S, k: u32) -> Option<String>
where
    S: ConcurrentCachePeekAsync<u32, String> + Sync,
    S::Error: std::fmt::Debug,
{
    s.async_cache_peek(&k)
        .await
        .expect("PeekableStore is infallible")
}

/// Same, but through the defaulted `async_peek` alias.
async fn peek_via_alias<S>(s: &S, k: u32) -> Option<String>
where
    S: ConcurrentCachePeekAsync<u32, String> + Sync,
    S::Error: std::fmt::Debug,
{
    s.async_peek(&k).await.expect("PeekableStore is infallible")
}

#[tokio::test]
async fn generic_bound_over_peek_async_reads_hit_and_miss() {
    let s = PeekableStore::default();
    s.insert(1, "ten");

    assert_eq!(peek_via_trait(&s, 1).await, Some("ten".to_string()));
    assert_eq!(
        peek_via_trait(&s, 2).await,
        None,
        "missing key peeks as None"
    );
}

#[tokio::test]
async fn async_peek_alias_delegates_to_async_cache_peek() {
    let s = PeekableStore::default();
    s.insert(1, "ten");

    let via_core = peek_via_trait(&s, 1).await;
    let peeks_after_core = s.peeks.load(Ordering::Relaxed);
    assert_eq!(peeks_after_core, 1);

    let via_alias = peek_via_alias(&s, 1).await;
    assert_eq!(
        via_alias, via_core,
        "the defaulted alias must return what `async_cache_peek` returns"
    );
    assert_eq!(
        s.peeks.load(Ordering::Relaxed),
        peeks_after_core + 1,
        "the defaulted `async_peek` must delegate to `async_cache_peek`, not reimplement it"
    );

    // Reachable fully-qualified through the trait as well.
    assert_eq!(
        ConcurrentCachePeekAsync::async_peek(&s, &2).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn peek_async_has_no_read_side_effects() {
    let s = PeekableStore::default();
    s.insert(1, "ten");

    // A side-effectful read bumps the counter...
    assert_eq!(s.counted_get(&1), Some("ten".to_string()));
    let reads = s.reads.load(Ordering::Relaxed);
    assert_eq!(reads, 1);

    // ...but peeking (core method and alias) must not.
    let _ = peek_via_trait(&s, 1).await;
    let _ = peek_via_alias(&s, 1).await;
    assert_eq!(
        s.reads.load(Ordering::Relaxed),
        reads,
        "async peek must be side-effect-free"
    );
}

#[tokio::test]
async fn peek_async_future_is_send_and_survives_a_spawn() {
    // Proves the RPIT carries `+ Send`: a non-`Send` future could not cross `tokio::spawn`.
    let s = std::sync::Arc::new(PeekableStore::default());
    s.insert(7, "seven");

    let s2 = std::sync::Arc::clone(&s);
    let got = tokio::spawn(async move { peek_via_trait(&*s2, 7).await })
        .await
        .unwrap();
    assert_eq!(got, Some("seven".to_string()));

    let s3 = std::sync::Arc::clone(&s);
    let got = tokio::spawn(async move { peek_via_alias(&*s3, 7).await })
        .await
        .unwrap();
    assert_eq!(got, Some("seven".to_string()));
}

// ── Prelude re-export ────────────────────────────────────────────────────────

#[tokio::test]
async fn peek_async_is_reachable_through_the_prelude() {
    // Deliberately shadow-free scope: only the prelude glob is imported here, so this fails to
    // compile if `ConcurrentCachePeekAsync` is missing from `cached::prelude`.
    mod via_prelude {
        #[allow(unused_imports)]
        use cached::prelude::*;

        pub async fn peek<S>(s: &S, k: u32) -> Option<String>
        where
            S: ConcurrentCachePeekAsync<u32, String> + Sync,
        {
            s.async_cache_peek(&k).await.ok().flatten()
        }
    }

    let s = PeekableStore::default();
    s.insert(3, "three");
    assert_eq!(via_prelude::peek(&s, 3).await, Some("three".to_string()));
    assert_eq!(via_prelude::peek(&s, 4).await, None);
}
