//! Proves that the `#[concurrent_cached]` autoref shim (`cached::__set_dispatch`)
//! picks the borrowed `SerializeCached::cache_set_ref` arm for stores that implement
//! `SerializeCached`, and falls back to the owned `ConcurrentCached::cache_set` (cloning
//! the value) for stores that do not. Clone counts are the observable signal.
//!
//! Part 3: sync clone-elision proof with custom in-memory stores.
//! Part 4: async clone-elision proof with custom `SerializeCachedAsync` vs
//! `ConcurrentCachedAsync`-only stores (0 clones on the borrowed arm, 1 on the fallback).

#![cfg(feature = "proc_macro")]

use std::sync::atomic::{AtomicUsize, Ordering};

static CLONES: AtomicUsize = AtomicUsize::new(0);

/// A value type that counts every Clone invocation via the global `CLONES` counter.
#[derive(PartialEq, Debug)]
struct Counted(u32);

impl Clone for Counted {
    fn clone(&self) -> Self {
        CLONES.fetch_add(1, Ordering::SeqCst);
        Counted(self.0)
    }
}

// Manual serialization: just store the u32 as a string. No serde needed.
fn counted_to_string(v: &Counted) -> String {
    v.0.to_string()
}

fn counted_from_str(s: &str) -> Counted {
    Counted(s.parse().expect("parse Counted"))
}

// ---------------------------------------------------------------------------
// SerStore: implements both ConcurrentCached AND SerializeCached.
// Backing storage is HashMap<u32, String>; cache_set_ref serializes from &V
// without ever cloning Counted.
// ---------------------------------------------------------------------------

mod stores {
    use super::{Counted, counted_from_str, counted_to_string};
    use cached::{ConcurrentCached, SerializeCached};
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::sync::Mutex;
    use std::time::Duration;

    pub struct SerStore {
        map: Mutex<HashMap<u32, String>>,
    }

    impl SerStore {
        pub fn new() -> Self {
            SerStore {
                map: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ConcurrentCached<u32, Counted> for SerStore {
        type Error = Infallible;

        fn cache_get(&self, k: &u32) -> Result<Option<Counted>, Infallible> {
            let map = self.map.lock().unwrap();
            Ok(map.get(k).map(|s| counted_from_str(s)))
        }

        fn cache_set(&self, k: u32, v: Counted) -> Result<Option<Counted>, Infallible> {
            let s = counted_to_string(&v);
            let mut map = self.map.lock().unwrap();
            Ok(map.insert(k, s).map(|s| counted_from_str(&s)))
        }

        fn cache_remove(&self, k: &u32) -> Result<Option<Counted>, Infallible> {
            let mut map = self.map.lock().unwrap();
            Ok(map.remove(k).map(|s| counted_from_str(&s)))
        }

        fn cache_remove_entry(&self, k: &u32) -> Result<Option<(u32, Counted)>, Infallible> {
            let mut map = self.map.lock().unwrap();
            Ok(map.remove_entry(k).map(|(k, s)| (k, counted_from_str(&s))))
        }

        fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
            false
        }

        fn cache_clear(&self) -> Result<(), Infallible> {
            self.map.lock().unwrap().clear();
            Ok(())
        }

        fn ttl(&self) -> Option<Duration> {
            None
        }
    }

    impl SerializeCached<u32, Counted> for SerStore {
        /// Serialize from &Counted — never clones Counted.
        fn cache_set_ref(&self, k: &u32, v: &Counted) -> Result<Option<Counted>, Infallible> {
            let s = counted_to_string(v);
            let mut map = self.map.lock().unwrap();
            Ok(map.insert(*k, s).map(|s| counted_from_str(&s)))
        }
    }

    // ---------------------------------------------------------------------------
    // OwnedStore: implements ONLY ConcurrentCached (NOT SerializeCached).
    // Backing storage is HashMap<u32, Counted>; cache_set stores the owned value.
    // Since SerializeCached is not implemented, the shim falls back to the owned
    // arm which clones the value once before calling cache_set.
    // ---------------------------------------------------------------------------

    pub struct OwnedStore {
        map: Mutex<HashMap<u32, Counted>>,
    }

    impl OwnedStore {
        pub fn new() -> Self {
            OwnedStore {
                map: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ConcurrentCached<u32, Counted> for OwnedStore {
        type Error = Infallible;

        fn cache_get(&self, k: &u32) -> Result<Option<Counted>, Infallible> {
            // Reading requires a Clone to return an owned value from the locked map.
            // We reset CLONES before each test and only care about the set-path clone.
            let map = self.map.lock().unwrap();
            Ok(map.get(k).cloned())
        }

        fn cache_set(&self, k: u32, v: Counted) -> Result<Option<Counted>, Infallible> {
            let mut map = self.map.lock().unwrap();
            Ok(map.insert(k, v))
        }

        fn cache_remove(&self, k: &u32) -> Result<Option<Counted>, Infallible> {
            let mut map = self.map.lock().unwrap();
            Ok(map.remove(k))
        }

        fn cache_remove_entry(&self, k: &u32) -> Result<Option<(u32, Counted)>, Infallible> {
            let mut map = self.map.lock().unwrap();
            Ok(map.remove_entry(k))
        }

        fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
            false
        }

        fn cache_clear(&self) -> Result<(), Infallible> {
            self.map.lock().unwrap().clear();
            Ok(())
        }

        fn ttl(&self) -> Option<Duration> {
            None
        }
    }

    // OwnedStore intentionally does NOT impl SerializeCached.
}

// ---------------------------------------------------------------------------
// Memoized functions using the two stores.
// ---------------------------------------------------------------------------

mod fns {
    use super::Counted;
    use super::stores::{OwnedStore, SerStore};
    use cached::macros::concurrent_cached;

    #[concurrent_cached(
        ty = "SerStore",
        create = "{ SerStore::new() }",
        map_error = "|e| e",
        key = "u32",
        convert = "{ n }"
    )]
    pub fn via_serialize(n: u32) -> Result<Counted, std::convert::Infallible> {
        Ok(Counted(n))
    }

    #[concurrent_cached(
        ty = "OwnedStore",
        create = "{ OwnedStore::new() }",
        map_error = "|e| e",
        key = "u32",
        convert = "{ n }"
    )]
    pub fn via_owned(n: u32) -> Result<Counted, std::convert::Infallible> {
        Ok(Counted(n))
    }
}

// Serialize each clone-counting test to avoid interference from parallel test
// execution sharing the global CLONES counter.
mod clone_count_tests {
    use super::*;
    use cached::ConcurrentCached;
    use fns::{VIA_OWNED, VIA_SERIALIZE, via_owned, via_serialize};
    use std::sync::Mutex;

    // A global mutex to serialize the two clone-counting tests so they don't
    // interleave and corrupt the shared CLONES counter.
    static CLONE_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Proves the borrowed arm (SerializeCached) is taken: no Clone of Counted occurs
    /// at the set site, so CLONES stays 0 after a cache miss.
    #[test]
    fn serialize_store_skips_value_clone() {
        let _guard = CLONE_TEST_LOCK.lock().unwrap();
        CLONES.store(0, Ordering::SeqCst);
        VIA_SERIALIZE.cache_clear().unwrap();

        // First call: cache miss; body runs, result stored via cache_set_ref (&Counted).
        let v = via_serialize(7).unwrap();
        assert_eq!(v, Counted(7));

        // The borrowed setter serialized from &Counted — no Clone.
        assert_eq!(
            CLONES.load(Ordering::SeqCst),
            0,
            "SerializeCached path must not clone the value at the set site"
        );

        // Second call: cache hit; deserializes to a fresh Counted (via counted_from_str,
        // not Clone), so CLONES is still 0.
        CLONES.store(0, Ordering::SeqCst);
        let hit = via_serialize(7).unwrap();
        assert_eq!(hit, Counted(7));
        assert_eq!(
            CLONES.load(Ordering::SeqCst),
            0,
            "Cache hit on SerializeCached path must not clone"
        );
    }

    /// Proves the owned fallback arm is taken: the shim clones the value once
    /// (to call the owned cache_set), so CLONES == 1 after a cache miss.
    /// On a cache hit, OwnedStore::cache_get calls `.cloned()` to return an owned
    /// value from the locked map. The counter is reset to 0 before the hit call,
    /// so the assertion CLONES == 1 measures exactly that one get-path clone, not
    /// any clone from the set-site shim (which is not invoked on a hit).
    #[test]
    fn owned_store_clones_once() {
        let _guard = CLONE_TEST_LOCK.lock().unwrap();
        CLONES.store(0, Ordering::SeqCst);
        VIA_OWNED.cache_clear().unwrap();

        // First call: cache miss; body runs, result stored via owned cache_set.
        // The shim does `value.clone()` before calling cache_set.
        let v = via_owned(7).unwrap();
        assert_eq!(v, Counted(7));

        assert_eq!(
            CLONES.load(Ordering::SeqCst),
            1,
            "Owned fallback path must clone the value exactly once at the set site"
        );

        // Second call: cache hit; the set-site shim is not invoked. CLONES is reset
        // to 0 here, so the assertion below measures the one clone from
        // OwnedStore::cache_get (which calls `.cloned()` to return an owned value).
        CLONES.store(0, Ordering::SeqCst);
        let hit = via_owned(7).unwrap();
        assert_eq!(hit, Counted(7));
        assert_eq!(
            CLONES.load(Ordering::SeqCst),
            1,
            "Cache hit: set-site shim not called; the one clone comes from cache_get returning an owned value"
        );
    }
}

// ---------------------------------------------------------------------------
// Part 4: async custom SerializeCachedAsync store round-trip.
// ---------------------------------------------------------------------------

#[cfg(feature = "async")]
mod async_serialize_store {
    use cached::{ConcurrentCachedAsync, SerializeCachedAsync};
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    // Counts every Clone of AsyncVal, so the borrowed-vs-owned async arm is observable
    // the same way `CLONES` makes it observable for the sync path.
    static ASYNC_CLONES: AtomicUsize = AtomicUsize::new(0);

    #[derive(PartialEq, Debug)]
    pub struct AsyncVal(u32);

    impl Clone for AsyncVal {
        fn clone(&self) -> Self {
            ASYNC_CLONES.fetch_add(1, Ordering::SeqCst);
            AsyncVal(self.0)
        }
    }

    fn async_val_to_string(v: &AsyncVal) -> String {
        v.0.to_string()
    }

    fn async_val_from_str(s: &str) -> AsyncVal {
        AsyncVal(s.parse().expect("parse AsyncVal"))
    }

    pub struct AsyncSerStore {
        map: Mutex<HashMap<u32, String>>,
    }

    impl AsyncSerStore {
        pub fn new() -> Self {
            AsyncSerStore {
                map: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ConcurrentCachedAsync<u32, AsyncVal> for AsyncSerStore {
        type Error = Infallible;

        fn async_cache_get(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            let result = {
                let map = self.map.lock().unwrap();
                map.get(k).map(|s| async_val_from_str(s))
            };
            async move { Ok(result) }
        }

        fn async_cache_set(
            &self,
            k: u32,
            v: AsyncVal,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            let s = async_val_to_string(&v);
            let prev = {
                let mut map = self.map.lock().unwrap();
                map.insert(k, s)
            };
            let prev_val = prev.map(|s| async_val_from_str(&s));
            async move { Ok(prev_val) }
        }

        fn async_cache_remove(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            let result = {
                let mut map = self.map.lock().unwrap();
                map.remove(k).map(|s| async_val_from_str(&s))
            };
            async move { Ok(result) }
        }

        fn async_cache_remove_entry(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<(u32, AsyncVal)>, Infallible>> + Send
        {
            let result = {
                let mut map = self.map.lock().unwrap();
                map.remove_entry(k)
                    .map(|(k, s)| (k, async_val_from_str(&s)))
            };
            async move { Ok(result) }
        }

        fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
            false
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

        fn ttl(&self) -> Option<Duration> {
            None
        }
    }

    impl SerializeCachedAsync<u32, AsyncVal> for AsyncSerStore {
        fn async_cache_set_ref(
            &self,
            k: &u32,
            v: &AsyncVal,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            let s = async_val_to_string(v);
            let k = *k;
            let prev = {
                let mut map = self.map.lock().unwrap();
                map.insert(k, s)
            };
            let prev_val = prev.map(|s| async_val_from_str(&s));
            async move { Ok(prev_val) }
        }
    }

    // AsyncOwnedStore: implements ONLY ConcurrentCachedAsync (NOT SerializeCachedAsync).
    // Backing storage is HashMap<u32, AsyncVal>; the shim must take the owned fallback arm,
    // cloning the value once before async_cache_set.
    pub struct AsyncOwnedStore {
        map: Mutex<HashMap<u32, AsyncVal>>,
    }

    impl AsyncOwnedStore {
        pub fn new() -> Self {
            AsyncOwnedStore {
                map: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ConcurrentCachedAsync<u32, AsyncVal> for AsyncOwnedStore {
        type Error = Infallible;

        fn async_cache_get(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            // Reading clones to return an owned value; the clone tests reset the counter
            // immediately before the set-triggering call, so a miss's get (None) does not
            // perturb the asserted set-path count.
            let result = {
                let map = self.map.lock().unwrap();
                map.get(k).cloned()
            };
            async move { Ok(result) }
        }

        fn async_cache_set(
            &self,
            k: u32,
            v: AsyncVal,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            let prev = {
                let mut map = self.map.lock().unwrap();
                map.insert(k, v)
            };
            async move { Ok(prev) }
        }

        fn async_cache_remove(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<AsyncVal>, Infallible>> + Send
        {
            let result = {
                let mut map = self.map.lock().unwrap();
                map.remove(k)
            };
            async move { Ok(result) }
        }

        fn async_cache_remove_entry(
            &self,
            k: &u32,
        ) -> impl std::future::Future<Output = Result<Option<(u32, AsyncVal)>, Infallible>> + Send
        {
            let result = {
                let mut map = self.map.lock().unwrap();
                map.remove_entry(k)
            };
            async move { Ok(result) }
        }

        fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
            false
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

        fn ttl(&self) -> Option<Duration> {
            None
        }
    }

    // AsyncOwnedStore intentionally does NOT impl SerializeCachedAsync.

    use cached::macros::concurrent_cached;

    #[concurrent_cached(
        ty = "AsyncSerStore",
        create = "{ AsyncSerStore::new() }",
        map_error = "|e| e",
        key = "u32",
        convert = "{ n }"
    )]
    async fn async_via_serialize(n: u32) -> Result<AsyncVal, Infallible> {
        Ok(AsyncVal(n))
    }

    #[concurrent_cached(
        ty = "AsyncOwnedStore",
        create = "{ AsyncOwnedStore::new() }",
        map_error = "|e| e",
        key = "u32",
        convert = "{ n }"
    )]
    async fn async_via_owned(n: u32) -> Result<AsyncVal, Infallible> {
        Ok(AsyncVal(n))
    }

    // Both async clone-counting tests share the global ASYNC_CLONES counter; serialize
    // them with serial_test so a concurrent reset cannot corrupt the asserted count.
    #[tokio::test]
    #[serial_test::serial(async_clones)]
    async fn async_serialize_store_skips_value_clone() {
        ASYNC_CLONES.store(0, Ordering::SeqCst);
        // The store is a lazily-initialized OnceCell; clear it only if a prior
        // test already initialized it, so the first call below is provably a miss.
        if let Some(store) = ASYNC_VIA_SERIALIZE.get() {
            store.async_cache_clear().await.unwrap();
        }

        // First call: cache miss, body runs, stored via async_cache_set_ref (&AsyncVal).
        let first = async_via_serialize(42).await.unwrap();
        assert_eq!(first, AsyncVal(42));

        // The borrowed async setter serialized from &AsyncVal -- no Clone at the set site.
        assert_eq!(
            ASYNC_CLONES.load(Ordering::SeqCst),
            0,
            "SerializeCachedAsync path must not clone the value at the set site"
        );

        // Second call: cache hit, body not re-run; deserializes (no Clone).
        ASYNC_CLONES.store(0, Ordering::SeqCst);
        let second = async_via_serialize(42).await.unwrap();
        assert_eq!(second, AsyncVal(42));
        assert_eq!(
            ASYNC_CLONES.load(Ordering::SeqCst),
            0,
            "Cache hit on SerializeCachedAsync path must not clone"
        );

        // A different key causes the body to run again, still via the borrowed setter.
        ASYNC_CLONES.store(0, Ordering::SeqCst);
        let other = async_via_serialize(99).await.unwrap();
        assert_eq!(other, AsyncVal(99));
        assert_eq!(ASYNC_CLONES.load(Ordering::SeqCst), 0);
    }

    /// Proves the owned async fallback arm is taken when the store does not implement
    /// SerializeCachedAsync: the shim clones the value once before async_cache_set.
    #[tokio::test]
    #[serial_test::serial(async_clones)]
    async fn async_owned_store_clones_once() {
        ASYNC_CLONES.store(0, Ordering::SeqCst);
        // The store is a lazily-initialized OnceCell; clear it only if a prior
        // test already initialized it, so the first call below is provably a miss.
        if let Some(store) = ASYNC_VIA_OWNED.get() {
            store.async_cache_clear().await.unwrap();
        }

        // First call: cache miss; the shim does `value.clone()` before async_cache_set.
        let v = async_via_owned(7).await.unwrap();
        assert_eq!(v, AsyncVal(7));

        assert_eq!(
            ASYNC_CLONES.load(Ordering::SeqCst),
            1,
            "Owned async fallback path must clone the value exactly once at the set site"
        );

        // Second call: cache hit; the set-site shim is not invoked. ASYNC_CLONES is
        // reset to 0 here, so the assertion below measures only the one clone from
        // AsyncOwnedStore::async_cache_get (which calls `.cloned()` to return an owned
        // value from the locked map).
        ASYNC_CLONES.store(0, Ordering::SeqCst);
        let hit = async_via_owned(7).await.unwrap();
        assert_eq!(hit, AsyncVal(7));
        assert_eq!(
            ASYNC_CLONES.load(Ordering::SeqCst),
            1,
            "Cache hit: set-site shim not called; the one clone comes from async_cache_get returning an owned value"
        );
    }
}
