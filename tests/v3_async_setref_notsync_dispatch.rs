//! Dispatch certification for the async set-site shim's `V: Sync` boundary.
//!
//! The preferred (borrowed, zero-clone) async arm in `cached::__set_dispatch_async`
//! requires `V: Sync`, because the returned future holds `&V` across its `.await`.
//! A `Send + !Sync + Clone` value therefore must NOT match the borrowed arm even
//! when the store implements `SerializeCachedAsync`; it must fall through to the
//! owned fallback arm, which clones the value exactly once before `async_cache_set`.
//!
//! `tests/serialize_set_dispatch.rs` proves the borrowed arm is taken for a `Sync`
//! value and the fallback for a store that does not implement `SerializeCachedAsync`.
//! Neither exercises the `!Sync` boundary: a store that DOES implement
//! `SerializeCachedAsync` but whose value is `!Sync`. This test closes that gap by
//! counting clones -- a regression that loosened the borrowed arm's `V: Sync` bound
//! (silently holding a `!Sync` `&V` across an await) would show 0 clones here.

#![cfg(all(feature = "async", feature = "proc_macro"))]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

// Counts every clone of NotSyncVal so the borrowed-vs-owned arm is observable.
static CLONES: AtomicUsize = AtomicUsize::new(0);

/// `Send + !Sync + Clone`. The `PhantomData<Cell<()>>` removes `Sync` (Cell is
/// `!Sync`) while staying `Send` (`Cell<()>: Send`). The custom `Clone` records
/// every clone in `CLONES`.
#[derive(Debug, PartialEq)]
pub struct NotSyncVal {
    n: u32,
    _not_sync: PhantomData<std::cell::Cell<()>>,
}

impl NotSyncVal {
    fn new(n: u32) -> Self {
        NotSyncVal {
            n,
            _not_sync: PhantomData,
        }
    }
}

impl Clone for NotSyncVal {
    fn clone(&self) -> Self {
        CLONES.fetch_add(1, Ordering::SeqCst);
        NotSyncVal::new(self.n)
    }
}

// Compile-time assertions: NotSyncVal is Send but not Sync.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<NotSyncVal>();
};
// (A `Sync` bound on NotSyncVal would fail to compile, which is the whole point.)

fn to_string(v: &NotSyncVal) -> String {
    v.n.to_string()
}
fn from_str(s: &str) -> NotSyncVal {
    NotSyncVal::new(s.parse().expect("parse NotSyncVal"))
}

use cached::{ConcurrentCacheBase, ConcurrentCachedAsync, SerializeCachedAsync};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;

/// Store that implements BOTH `ConcurrentCachedAsync` and `SerializeCachedAsync`
/// for a `!Sync` value. Because the value is `!Sync`, the async set-site shim must
/// still pick the owned fallback (one clone), never the borrowed arm.
pub struct NotSyncStore {
    map: Mutex<HashMap<u32, String>>,
}

impl NotSyncStore {
    pub fn new() -> Self {
        NotSyncStore {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for NotSyncStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConcurrentCacheBase for NotSyncStore {
    type Error = Infallible;
}

impl ConcurrentCachedAsync<u32, NotSyncVal> for NotSyncStore {
    fn async_cache_get(
        &self,
        k: &u32,
    ) -> impl std::future::Future<Output = Result<Option<NotSyncVal>, Infallible>> + Send {
        let result = {
            let map = self.map.lock().unwrap();
            map.get(k).map(|s| from_str(s))
        };
        async move { Ok(result) }
    }

    fn async_cache_set(
        &self,
        k: u32,
        v: NotSyncVal,
    ) -> impl std::future::Future<Output = Result<Option<NotSyncVal>, Infallible>> + Send {
        let s = to_string(&v);
        let prev = {
            let mut map = self.map.lock().unwrap();
            map.insert(k, s)
        };
        let prev_val = prev.map(|s| from_str(&s));
        async move { Ok(prev_val) }
    }

    fn async_cache_remove(
        &self,
        k: &u32,
    ) -> impl std::future::Future<Output = Result<Option<NotSyncVal>, Infallible>> + Send {
        let result = {
            let mut map = self.map.lock().unwrap();
            map.remove(k).map(|s| from_str(&s))
        };
        async move { Ok(result) }
    }

    fn async_cache_remove_entry(
        &self,
        k: &u32,
    ) -> impl std::future::Future<Output = Result<Option<(u32, NotSyncVal)>, Infallible>> + Send
    {
        let result = {
            let mut map = self.map.lock().unwrap();
            map.remove_entry(k).map(|(k, s)| (k, from_str(&s)))
        };
        async move { Ok(result) }
    }

    fn async_cache_clear(
        &self,
    ) -> impl std::future::Future<Output = Result<(), Infallible>> + Send
    where
        Self: Sync,
    {
        self.map.lock().unwrap().clear();
        async move { Ok(()) }
    }

    fn async_cache_reset(
        &self,
    ) -> impl std::future::Future<Output = Result<(), Infallible>> + Send
    where
        Self: Sync,
    {
        self.map.lock().unwrap().clear();
        async move { Ok(()) }
    }
}

impl SerializeCachedAsync<u32, NotSyncVal> for NotSyncStore {
    fn async_cache_set_ref(
        &self,
        k: &u32,
        v: &NotSyncVal,
    ) -> impl std::future::Future<Output = Result<(), Infallible>> + Send {
        // Serialize from &NotSyncVal eagerly (before the await): the `&V` is never
        // held across the await, so this future is Send with only `V: Send`.
        let s = to_string(v);
        let k = *k;
        self.map.lock().unwrap().insert(k, s);
        async move { Ok(()) }
    }
}

use cached::macros::concurrent_cached;

#[concurrent_cached(
    ty = "NotSyncStore",
    create = "{ NotSyncStore::new() }",
    map_error = "|e| e",
    key = "u32",
    convert = "{ n }"
)]
async fn via_not_sync(n: u32) -> Result<NotSyncVal, Infallible> {
    Ok(NotSyncVal::new(n))
}

/// A `!Sync` value forces the owned fallback arm even though the store implements
/// `SerializeCachedAsync`: exactly one clone at the set site on a cache miss.
#[tokio::test]
#[serial_test::serial(notsync_clones)]
async fn not_sync_value_takes_owned_fallback_one_clone() {
    CLONES.store(0, Ordering::SeqCst);
    if let Some(store) = VIA_NOT_SYNC.get() {
        store.async_cache_clear().await.unwrap();
    }

    // Cache miss: body runs, result stored. The borrowed arm needs V: Sync, which
    // NotSyncVal is not, so the shim clones once for the owned async_cache_set.
    let v = via_not_sync(7).await.unwrap();
    assert_eq!(v, NotSyncVal::new(7));
    assert_eq!(
        CLONES.load(Ordering::SeqCst),
        1,
        "a !Sync value must take the owned fallback arm and clone exactly once at the set site"
    );

    // Cache hit: the set-site shim is not invoked. Only correctness is asserted.
    CLONES.store(0, Ordering::SeqCst);
    let hit = via_not_sync(7).await.unwrap();
    assert_eq!(hit, NotSyncVal::new(7));
}
