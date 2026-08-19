//! Formal tests for ExpiringCache::cache_get_or_set_with_mut on_evict ordering (C4).
//!
//! Contract: the replacement is installed FIRST, then the eviction is counted, then
//! on_evict fires with the displaced value. This matches `cache_set` / `TtlCache::set_entry`
//! and is what makes the path panic-safe: a callback that unwinds cannot leave the stale
//! entry in the slot, counted, for the next call to count and clean up a second time.
//! `on_evict` is `Fn(&K, &V)` and receives no handle on the cache, so nothing observable
//! depends on the old value still occupying the slot while it runs.
//!
//! How the distinguishing assertion works
//! ----------------------------------------
//! Before calling cache_get_or_set_with_mut we obtain a raw pointer to the value
//! currently in the HashMap slot (via cache_get_mut on a non-expired entry).  We
//! then mark that value as expired (so the next get-or-set sees it as stale) and
//! record the pointer.
//!
//! Inside the on_evict callback we record the address of the &V argument.
//!
//! Broken code (on_evict before insert):
//!   occupied.get() returns &V pointing directly at the map slot, so the callback
//!   address equals the pre-call slot address -- the stale entry is still installed.
//!
//! Fixed code (insert before on_evict):
//!   OccupiedEntry::insert(new_val) performs a mem::replace at the slot address,
//!   writing new_val into the slot and returning the old value as a moved local.
//!   The &V argument to on_evict is then &old (that local), whose address differs
//!   from the map slot -- and the slot itself (the returned &mut V) is unchanged and
//!   already holds the new value.
//!
//! The tests therefore fail on the pre-fix implementation and pass on the fix.

use std::sync::{Arc, Mutex};

use cached::{Cached, Expires, ExpiringCache};

// ---------------------------------------------------------------------------
// Shared value type
// ---------------------------------------------------------------------------

struct Val {
    id: u32,
    expired: bool,
}

impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Insert a non-expired Val into the cache, capture the raw slot pointer, then
/// flip the entry to expired so the next get-or-set triggers eviction.
fn insert_and_expire(cache: &mut ExpiringCache<u32, Val>, key: u32, id: u32) -> usize {
    cache.cache_set(key, Val { id, expired: false });
    let v_ref = cache
        .cache_get_mut(&key)
        .expect("value was just inserted and is not yet expired");
    v_ref.expired = true;
    v_ref as *mut Val as usize
}

// ---------------------------------------------------------------------------
// cache_get_or_set_with_mut
// ---------------------------------------------------------------------------

/// on_evict must fire with a &V that is the DISPLACED value, proving the replacement
/// already landed in the map slot at the time the callback runs.
#[test]
fn on_evict_fires_after_insert_in_get_or_set_with_mut() {
    let captured_ptr: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let captured_ptr_clone = captured_ptr.clone();
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_evict = events.clone();

    let mut cache = ExpiringCache::<u32, Val>::builder()
        .on_evict(move |_k, v| {
            events_for_evict.lock().unwrap().push("evict");
            *captured_ptr_clone.lock().unwrap() = Some(v as *const Val as usize);
        })
        .build()
        .unwrap();

    let old_ptr = insert_and_expire(&mut cache, 1, 10);

    let returned = cache.cache_get_or_set_with_mut(1, || {
        events.lock().unwrap().push("factory");
        Val {
            id: 20,
            expired: false,
        }
    });

    // Returned reference must be the new value.
    assert_eq!(
        returned.id, 20,
        "returned reference must point to the new value"
    );

    // factory runs before on_evict (ordering side channel).
    {
        let ev = events.lock().unwrap();
        assert_eq!(
            *ev,
            vec!["factory", "evict"],
            "factory must run before on_evict fires"
        );
    }

    // The map slot itself is untouched by the replacement (mem::replace in place) and
    // already holds the new value when the callback runs.
    assert_eq!(
        returned as *const Val as usize, old_ptr,
        "the slot address is stable across the replacement"
    );

    // The callback's &V argument must be the displaced local, NOT the map slot.
    // Pre-fix: occupied.get() IS the map slot, so the addresses match.
    let cb_ptr = captured_ptr
        .lock()
        .unwrap()
        .expect("on_evict must have fired");
    assert_ne!(
        cb_ptr, old_ptr,
        "on_evict must fire AFTER the replacement, with the displaced value \
         (callback &V address must differ from the map slot address)"
    );
}

/// Callback argument must be the old value's id, not the new one.
#[test]
fn on_evict_callback_arg_is_old_value_in_get_or_set_with_mut() {
    let evict_id: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let evict_id_clone = evict_id.clone();

    let mut cache = ExpiringCache::<u32, Val>::builder()
        .on_evict(move |_k, v| {
            *evict_id_clone.lock().unwrap() = Some(v.id);
        })
        .build()
        .unwrap();

    cache.cache_set(
        1,
        Val {
            id: 10,
            expired: true,
        },
    );
    cache.cache_get_or_set_with_mut(1, || Val {
        id: 20,
        expired: false,
    });

    assert_eq!(
        *evict_id.lock().unwrap(),
        Some(10),
        "on_evict callback must receive the OLD value (id=10), not the new one (id=20)"
    );
}

// ---------------------------------------------------------------------------
// cache_try_get_or_set_with_mut
// ---------------------------------------------------------------------------

/// Same slot-pointer ordering check for the fallible try variant.
#[test]
fn on_evict_fires_after_insert_in_try_get_or_set_with_mut() {
    let captured_ptr: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let captured_ptr_clone = captured_ptr.clone();
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_evict = events.clone();

    let mut cache = ExpiringCache::<u32, Val>::builder()
        .on_evict(move |_k, v| {
            events_for_evict.lock().unwrap().push("evict");
            *captured_ptr_clone.lock().unwrap() = Some(v as *const Val as usize);
        })
        .build()
        .unwrap();

    let old_ptr = insert_and_expire(&mut cache, 1, 10);

    let result: Result<&mut Val, std::convert::Infallible> =
        cache.cache_try_get_or_set_with_mut(1, || {
            events.lock().unwrap().push("factory");
            Ok(Val {
                id: 20,
                expired: false,
            })
        });
    let returned = result.expect("infallible factory cannot fail");

    assert_eq!(
        returned.id, 20,
        "returned reference must point to the new value"
    );

    {
        let ev = events.lock().unwrap();
        assert_eq!(
            *ev,
            vec!["factory", "evict"],
            "factory must run before on_evict fires (try variant)"
        );
    }

    assert_eq!(
        returned as *const Val as usize, old_ptr,
        "the slot address is stable across the replacement"
    );

    let cb_ptr = captured_ptr
        .lock()
        .unwrap()
        .expect("on_evict must have fired");
    assert_ne!(
        cb_ptr, old_ptr,
        "on_evict must fire AFTER the replacement, with the displaced value \
         (try variant: callback &V address must differ from the map slot address)"
    );
}

/// Callback arg is old value id in the try variant.
#[test]
fn on_evict_callback_arg_is_old_value_in_try_get_or_set_with_mut() {
    let evict_id: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let evict_id_clone = evict_id.clone();

    let mut cache = ExpiringCache::<u32, Val>::builder()
        .on_evict(move |_k, v| {
            *evict_id_clone.lock().unwrap() = Some(v.id);
        })
        .build()
        .unwrap();

    cache.cache_set(
        1,
        Val {
            id: 10,
            expired: true,
        },
    );
    let _: Result<_, std::convert::Infallible> = cache.cache_try_get_or_set_with_mut(1, || {
        Ok(Val {
            id: 20,
            expired: false,
        })
    });

    assert_eq!(
        *evict_id.lock().unwrap(),
        Some(10),
        "on_evict callback must receive the OLD value (id=10) in try variant"
    );
}

// ---------------------------------------------------------------------------
// Async variants (async_core feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "async_core")]
mod async_tests {
    use super::{Val, insert_and_expire};
    use cached::{CachedGetOrSetAsync, ExpiringCache};
    use std::sync::{Arc, Mutex};

    /// Same slot-pointer ordering check for async_cache_get_or_set_with_mut.
    #[tokio::test]
    async fn on_evict_fires_after_insert_in_async_get_or_set_with_mut() {
        let captured_ptr: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let captured_ptr_clone = captured_ptr.clone();
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let events_for_evict = events.clone();

        let mut cache = ExpiringCache::<u32, Val>::builder()
            .on_evict(move |_k, v| {
                events_for_evict.lock().unwrap().push("evict");
                *captured_ptr_clone.lock().unwrap() = Some(v as *const Val as usize);
            })
            .build()
            .unwrap();

        let old_ptr = insert_and_expire(&mut cache, 1, 10);

        let returned = cache
            .async_cache_get_or_set_with_mut(1, || async {
                events.lock().unwrap().push("factory");
                Val {
                    id: 20,
                    expired: false,
                }
            })
            .await;

        assert_eq!(
            returned.id, 20,
            "returned reference must point to the new value"
        );

        {
            let ev = events.lock().unwrap();
            assert_eq!(
                *ev,
                vec!["factory", "evict"],
                "factory must run before on_evict fires (async variant)"
            );
        }

        assert_eq!(
            returned as *const Val as usize, old_ptr,
            "the slot address is stable across the replacement"
        );

        let cb_ptr = captured_ptr
            .lock()
            .unwrap()
            .expect("on_evict must have fired");
        assert_ne!(
            cb_ptr, old_ptr,
            "async on_evict must fire AFTER the replacement, with the displaced value"
        );
    }

    /// Same slot-pointer ordering check for async_cache_try_get_or_set_with_mut.
    #[tokio::test]
    async fn on_evict_fires_after_insert_in_async_try_get_or_set_with_mut() {
        let captured_ptr: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let captured_ptr_clone = captured_ptr.clone();
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let events_for_evict = events.clone();

        let mut cache = ExpiringCache::<u32, Val>::builder()
            .on_evict(move |_k, v| {
                events_for_evict.lock().unwrap().push("evict");
                *captured_ptr_clone.lock().unwrap() = Some(v as *const Val as usize);
            })
            .build()
            .unwrap();

        let old_ptr = insert_and_expire(&mut cache, 1, 10);

        let result: Result<&mut Val, std::convert::Infallible> = cache
            .async_cache_try_get_or_set_with_mut(1, || async {
                events.lock().unwrap().push("factory");
                Ok(Val {
                    id: 20,
                    expired: false,
                })
            })
            .await;
        let returned = result.expect("infallible factory cannot fail");

        assert_eq!(
            returned.id, 20,
            "returned reference must point to the new value"
        );

        {
            let ev = events.lock().unwrap();
            assert_eq!(
                *ev,
                vec!["factory", "evict"],
                "factory must run before on_evict fires (async try variant)"
            );
        }

        assert_eq!(
            returned as *const Val as usize, old_ptr,
            "the slot address is stable across the replacement"
        );

        let cb_ptr = captured_ptr
            .lock()
            .unwrap()
            .expect("on_evict must have fired");
        assert_ne!(
            cb_ptr, old_ptr,
            "async try on_evict must fire AFTER the replacement, with the displaced value"
        );
    }
}
