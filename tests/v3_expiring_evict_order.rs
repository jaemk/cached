//! Formal tests for ExpiringCache::cache_get_or_set_with_mut on_evict ordering (C4).
//!
//! Contract: on_evict fires while the old entry is still in the map slot, BEFORE
//! the new value is inserted. This matches TtlCache semantics.
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
//! Broken code (insert before on_evict):
//!   OccupiedEntry::insert(new_val) performs a mem::replace at the slot address,
//!   writing new_val into the slot and returning the old value as a moved local.
//!   The &V argument to on_evict is then &old (the local), whose address differs
//!   from the original map slot.
//!
//! Fixed code (on_evict before insert):
//!   occupied.get() returns &V pointing directly at the map slot.  The address
//!   matches the pointer captured before the call.
//!
//! The test therefore fails on the broken implementation and passes on the fix.

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

/// on_evict must fire with a &V that points to the original map slot, proving
/// the old entry is still present at the time the callback runs.
#[test]
fn on_evict_fires_before_insert_in_get_or_set_with_mut() {
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

    // The callback's &V argument must point to the same slot as before the call.
    // Broken code: &old (displaced local) has a different address than old_ptr.
    // Fixed code: occupied.get() IS the map slot, so it matches old_ptr.
    let cb_ptr = captured_ptr
        .lock()
        .unwrap()
        .expect("on_evict must have fired");
    assert_eq!(
        cb_ptr, old_ptr,
        "on_evict must fire while the old entry is still in the map slot \
         (callback &V address must match the pre-call slot address)"
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
fn on_evict_fires_before_insert_in_try_get_or_set_with_mut() {
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

    let cb_ptr = captured_ptr
        .lock()
        .unwrap()
        .expect("on_evict must have fired");
    assert_eq!(
        cb_ptr, old_ptr,
        "on_evict must fire while the old entry is still in the map slot \
         (try variant: callback &V address must match the pre-call slot address)"
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
    async fn on_evict_fires_before_insert_in_async_get_or_set_with_mut() {
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

        let cb_ptr = captured_ptr
            .lock()
            .unwrap()
            .expect("on_evict must have fired");
        assert_eq!(
            cb_ptr, old_ptr,
            "async on_evict must fire while the old entry is still in the map slot"
        );
    }

    /// Same slot-pointer ordering check for async_cache_try_get_or_set_with_mut.
    #[tokio::test]
    async fn on_evict_fires_before_insert_in_async_try_get_or_set_with_mut() {
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

        let cb_ptr = captured_ptr
            .lock()
            .unwrap()
            .expect("on_evict must have fired");
        assert_eq!(
            cb_ptr, old_ptr,
            "async try on_evict must fire while old entry is still in the map slot"
        );
    }
}
