//! Single-flight claims on a key, released when the claim is dropped.
//!
//! A [`ClaimRegistry`] holds the set of keys that currently have work in flight.
//! [`ClaimRegistry::claim`] hands the first caller a [`Claim`] and every later caller `None`,
//! until that claim is dropped. It exists for the background-refresh recipes, where every reader
//! that observes one stale entry would otherwise start its own recompute; see
//! `examples/stale_while_revalidate.rs` and `examples/refresh_before_expiry.rs`.
//!
//! The registry is independent of any store, so it works with `#[cached]`,
//! `#[concurrent_cached]`, and hand-rolled caches alike. It spawns nothing and awaits nothing:
//! spawning the refresh stays with the caller.
//!
//! ```rust
//! use cached::claim::ClaimRegistry;
//!
//! let registry: ClaimRegistry<String> = ClaimRegistry::new();
//!
//! let claim = registry.claim("user:1".to_string()).expect("first caller wins");
//! assert_eq!(claim.key(), "user:1");
//! assert!(registry.claim("user:1".to_string()).is_none(), "already in flight");
//!
//! drop(claim);
//! assert!(registry.is_empty());
//! assert!(registry.claim("user:1".to_string()).is_some(), "released, so claimable again");
//! ```
//!
//! The shape the refresh recipes use: claim, move the claim into the spawned refresh, and let
//! the refresh's end drop it. Borrowing the key out of the guard with [`Claim::key`] keeps the
//! guard alive for exactly the refresh, so the release cannot be forgotten.
//!
//! ```rust
//! use cached::claim::ClaimRegistry;
//! use std::sync::LazyLock;
//!
//! # fn prime_cache(_key: &str) {}
//! static REFRESHING: LazyLock<ClaimRegistry<String>> = LazyLock::new(ClaimRegistry::new);
//!
//! fn refresh(key: &str) {
//!     // Claim after deciding to refresh, never before.
//!     if let Some(claim) = REFRESHING.claim(key.to_string()) {
//!         std::thread::spawn(move || {
//!             prime_cache(claim.key());
//!             // `claim` is dropped here, releasing the key.
//!         });
//!     }
//! }
//! # refresh("user:1");
//! ```
//!
//! # Release paths
//!
//! The key is released by [`Drop`], which covers all three ways the work can end: normal
//! completion, an unwind out of the body, and cancellation (an async task whose future is
//! dropped mid-poll, for example a `tokio` `JoinHandle::abort`). A hand-written release call at
//! the end of the body covers only the first, and a key left claimed after a panic or an abort
//! is never refreshed again for the life of the process.
//!
//! The mutex is [`parking_lot::Mutex`], which does not poison, so the release path cannot panic
//! a second time while a panic is already unwinding.
//!
//! # Do not leak a claim
//!
//! `mem::forget`, `Box::leak`, or parking a claim in a long-lived collection wedges that key
//! permanently: no later caller can ever claim it. That is the same failure as a missed release,
//! reintroduced from the other end. Bind the claim for the work and let it drop.
//!
//! Claiming on a path where no work follows is a bug for the same reason: it turns away the
//! caller that would have done the work. Claim after the decision to do the work, never before
//! it. [`ClaimRegistry::len`] and [`ClaimRegistry::is_empty`] are there so tests can assert the
//! registry drains.
//!
//! # A claim is not a lock
//!
//! The registry mutex is held only across the set insert or remove, never across the work
//! itself. A caller that does not get the claim is not blocked; it is told `None` and moves on,
//! which for a stale-while-revalidate read means serving the stale value immediately. A guard
//! that held a lock across the recompute would serialize readers on every key, which is exactly
//! what these recipes exist to avoid.
//!
//! # It does not deduplicate the cold path
//!
//! With nothing cached there is no value for the `None` callers to serve, so a claim is the
//! wrong tool: the losers would return empty-handed. `sync_writes = "by_key"` is what
//! deduplicates a cold key, and it covers the store write rather than the function body. The two
//! compose (claim the refresh, `sync_writes` the cold miss); neither substitutes for the other.
//!
//! # Capacity
//!
//! The key set keeps its peak allocation for the life of the registry: it is never shrunk, so a
//! burst that holds N simultaneous claims retains a table sized for N even after it drains to
//! empty. The keys themselves are dropped on release, and the peak is bounded by the number of
//! claims in flight at once (concurrency), not by the size of the key space. Use a registry per
//! key space rather than one global registry if that bound matters.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

use parking_lot::Mutex;

/// The set of keys that currently have work in flight.
///
/// Cheap to clone: every clone shares one set, so a registry can be held in a struct field, in a
/// `LazyLock` static, or cloned into a task. See the [module docs](self) for usage and for the
/// things that bite (leaked claims, the cold path, and what a claim is not).
///
/// `K: Clone` because the set owns one copy of the key and the [`Claim`] owns the copy that
/// [`Claim::key`] hands to the work. There is no value type and no store type in the signature.
pub struct ClaimRegistry<K> {
    keys: Arc<Mutex<HashSet<K>>>,
}

impl<K> Clone for ClaimRegistry<K> {
    /// Returns another handle to the same set of claims.
    fn clone(&self) -> Self {
        Self {
            keys: Arc::clone(&self.keys),
        }
    }
}

impl<K: Eq + Hash + Clone> Default for ClaimRegistry<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone> ClaimRegistry<K> {
    /// Creates an empty registry.
    ///
    /// Not `const`, so a `static` registry needs a [`LazyLock`](std::sync::LazyLock):
    ///
    /// ```rust
    /// use cached::claim::ClaimRegistry;
    /// use std::sync::LazyLock;
    ///
    /// static REFRESHING: LazyLock<ClaimRegistry<String>> = LazyLock::new(ClaimRegistry::new);
    /// assert!(REFRESHING.is_empty());
    /// ```
    pub fn new() -> Self {
        Self {
            keys: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Claims `key` for this caller, or returns `None` if a claim on `key` is already live.
    ///
    /// Claim after deciding to do the work, never before it: a claim taken on a path that does
    /// not go on to do the work turns away the caller that would have.
    ///
    /// ```rust
    /// use cached::claim::ClaimRegistry;
    ///
    /// let registry = ClaimRegistry::new();
    /// let claim = registry.claim(7).unwrap();
    /// assert!(registry.claim(7).is_none());
    /// assert!(registry.claim(8).is_some(), "a different key is unaffected");
    /// drop(claim);
    /// assert!(registry.claim(7).is_some());
    /// ```
    #[must_use = "the claim releases the key when dropped; bind it for the whole refresh"]
    pub fn claim(&self, key: K) -> Option<Claim<K>> {
        {
            let mut keys = self.keys.lock();
            if !keys.insert(key.clone()) {
                return None;
            }
        }
        Some(Claim {
            registry: self.clone(),
            key,
        })
    }

    /// Returns `true` while a claim on `key` is live.
    ///
    /// Accepts any borrowed form of the key, so a `ClaimRegistry<String>` is queried with a
    /// `&str`:
    ///
    /// ```rust
    /// use cached::claim::ClaimRegistry;
    ///
    /// let registry: ClaimRegistry<String> = ClaimRegistry::new();
    /// let claim = registry.claim("user:1".to_string()).unwrap();
    /// assert!(registry.is_claimed("user:1"));
    /// drop(claim);
    /// assert!(!registry.is_claimed("user:1"));
    /// ```
    ///
    /// The answer is a snapshot: another thread can claim or release `key` before the caller
    /// acts on it. Use [`claim`](ClaimRegistry::claim) itself, whose insert is atomic, to decide
    /// whether to do the work.
    pub fn is_claimed<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.keys.lock().contains(key)
    }

    /// Returns the number of live claims.
    pub fn len(&self) -> usize {
        self.keys.lock().len()
    }

    /// Returns `true` when no claim is live.
    pub fn is_empty(&self) -> bool {
        self.keys.lock().is_empty()
    }
}

impl<K: fmt::Debug> fmt::Debug for ClaimRegistry<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimRegistry")
            .field("claimed", &*self.keys.lock())
            .finish()
    }
}

/// A live claim on one key, released when dropped.
///
/// Obtained from [`ClaimRegistry::claim`]. It owns a handle to the registry, so it is `'static`
/// for a `'static` key and can be moved into a spawned thread or task; it is `Send` when
/// `K: Send`, which is what `tokio::spawn` requires.
///
/// Deliberately not [`Clone`]: two live claims on one key is the thing the type prevents. Also
/// do not leak it (see the [module docs](self)), which wedges the key permanently.
pub struct Claim<K: Eq + Hash> {
    registry: ClaimRegistry<K>,
    key: K,
}

impl<K: Eq + Hash> Claim<K> {
    /// Returns the claimed key.
    ///
    /// Passing this borrow to the work keeps the claim alive for exactly the work: the guard
    /// cannot be dropped early without a borrow error.
    pub fn key(&self) -> &K {
        &self.key
    }
}

impl<K: Eq + Hash> Drop for Claim<K> {
    /// Releases the key. Runs on completion, on an unwind, and on cancellation alike.
    fn drop(&mut self) {
        self.registry.keys.lock().remove(&self.key);
    }
}

impl<K: Eq + Hash + fmt::Debug> fmt::Debug for Claim<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claim").field("key", &self.key).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_claim_of_a_live_key_is_none() {
        let registry = ClaimRegistry::new();
        let claim = registry.claim("a".to_string()).expect("first claim wins");
        assert!(registry.claim("a".to_string()).is_none());
        assert_eq!(claim.key(), "a");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn drop_releases_the_key_and_a_re_claim_succeeds() {
        let registry = ClaimRegistry::new();
        let claim = registry.claim("a".to_string()).unwrap();
        drop(claim);
        assert!(!registry.is_claimed("a"));
        assert!(registry.claim("a".to_string()).is_some());
    }

    #[test]
    fn distinct_keys_are_claimed_independently_and_drain_to_empty() {
        let registry = ClaimRegistry::new();
        let a = registry.claim("a".to_string()).unwrap();
        let b = registry.claim("b".to_string()).unwrap();
        assert_eq!(registry.len(), 2);

        drop(a);
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_claimed("a"));
        assert!(registry.is_claimed("b"));

        drop(b);
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn is_claimed_accepts_a_borrowed_key() {
        let registry: ClaimRegistry<String> = ClaimRegistry::new();
        assert!(!registry.is_claimed("a"));
        let claim = registry.claim("a".to_string()).unwrap();
        // Queried through `&str`, not `&String`.
        let borrowed: &str = "a";
        assert!(registry.is_claimed(borrowed));
        drop(claim);
        assert!(!registry.is_claimed(borrowed));
    }

    #[test]
    fn a_clone_of_the_registry_shares_the_claims() {
        let registry = ClaimRegistry::new();
        let other = registry.clone();
        let claim = registry.claim(1).unwrap();
        assert!(other.is_claimed(&1));
        assert!(other.claim(1).is_none());
        drop(claim);
        assert!(other.is_empty());
    }

    #[test]
    fn a_claim_outlives_the_registry_handle_it_came_from() {
        let shared = ClaimRegistry::new();
        let claim = {
            let handle = shared.clone();
            handle.claim(1).unwrap()
        };
        assert!(shared.is_claimed(&1));
        drop(claim);
        assert!(shared.is_empty());
    }

    #[test]
    fn a_claim_is_send_for_a_send_key() {
        fn assert_send<T: Send>(_: &T) {}
        let registry: ClaimRegistry<String> = ClaimRegistry::new();
        let claim = registry.claim("a".to_string()).unwrap();
        assert_send(&registry);
        assert_send(&claim);
    }

    #[test]
    fn default_is_an_empty_registry() {
        let registry: ClaimRegistry<u32> = ClaimRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }
}
