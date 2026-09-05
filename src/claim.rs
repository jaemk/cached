//! Single-flight claims on a key, released when the claim is dropped.
//!
//! A [`ClaimRegistry`] holds the set of keys that currently have work in flight.
//! [`ClaimRegistry::claim`] hands the first caller a [`Claim`] and every later caller `None`,
//! until that claim is dropped. It exists for the background-refresh recipes, where every reader
//! that observes one stale entry would otherwise start its own recompute; see
//! [`examples/stale_while_revalidate.rs`](https://github.com/jaemk/cached/blob/master/examples/stale_while_revalidate.rs)
//! and `examples/refresh_before_expiry.rs`.
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
//! guard alive for the duration of the call the borrow is passed into, so the release cannot be
//! forgotten while that call runs.
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
//! None of that covers the work simply never finishing. A claim is held for as long as the work
//! runs, with no upper bound: if the refresh hangs (a stuck network call, a deadlock in the work
//! itself), the key stays claimed for the life of the process. Every later caller sees `None` and
//! keeps serving the stale value forever, and no caller ever gets to recompute that key again.
//! Bound the work with a timeout so a hang still reaches the claim's drop; see the
//! `tokio::time::timeout` in
//! [`examples/stale_while_revalidate.rs`](https://github.com/jaemk/cached/blob/master/examples/stale_while_revalidate.rs).
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
//! # Do not release a claim early
//!
//! The opposite mistake is the more likely one, and it is silent: the claim is dropped before
//! the work it is meant to cover starts, so every caller wins its own claim and the
//! deduplication is gone. No lint fires on any of these: `#[must_use]` catches only a value
//! left unused, and each of these consumes the claim, then drops it too soon.
//!
//! ```rust
//! use cached::claim::ClaimRegistry;
//!
//! # fn spawn_refresh(_key: String) {}
//! # let registry: ClaimRegistry<String> = ClaimRegistry::new();
//! # let key = "user:1";
//! // Wrong: the temporary claim is dropped at the end of the condition, before the work runs.
//! if registry.claim(key.to_string()).is_some() {
//!     spawn_refresh(key.to_string());
//! }
//!
//! // Wrong: `let _ =` drops the claim immediately. `let _claim = ...` would not.
//! let _ = registry.claim(key.to_string());
//!
//! // Wrong: the work captures the key but not the claim, so the claim drops at the end of the
//! // block. This is the natural shape for a caller that already has the key and so never
//! // reaches for `Claim::key`.
//! if let Some(claim) = registry.claim(key.to_string()) {
//!     let owned = claim.key().clone();
//!     spawn_refresh(owned);
//! }
//! ```
//!
//! The claim has to be bound and moved into the work, so that the work's end is what drops it:
//!
//! ```rust
//! use cached::claim::ClaimRegistry;
//!
//! # fn prime_cache(_key: &str) {}
//! let registry: ClaimRegistry<String> = ClaimRegistry::new();
//!
//! if let Some(claim) = registry.claim("user:1".to_string()) {
//!     std::thread::spawn(move || {
//!         prime_cache(claim.key());
//!         // `claim` is dropped here, at the end of the work, releasing the key.
//!     });
//! }
//! ```
//!
//! The distinguishing detail is that the claim was never captured, not that cloning the key is
//! itself wrong. A refresh function that takes `K` by value (rather than `&K`, as `prime_cache`
//! above does) still has to move the *claim* into the closure; it clones the key out of the claim
//! from inside the work, after the move, so the clone happens while the claim is still live:
//!
//! ```rust
//! use cached::claim::ClaimRegistry;
//!
//! # fn prime_cache_owned(_key: String) {}
//! let registry: ClaimRegistry<String> = ClaimRegistry::new();
//!
//! if let Some(claim) = registry.claim("user:1".to_string()) {
//!     std::thread::spawn(move || {
//!         // The claim moved into the closure; the clone happens after that move, so the claim
//!         // is still held while `prime_cache_owned` runs.
//!         prime_cache_owned(claim.key().clone());
//!         // `claim` is dropped here, at the end of the work, releasing the key.
//!     });
//! }
//! ```
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
//!
//! There is no `shrink_to_fit` or `clear` on [`ClaimRegistry`], and none is planned: reclaiming
//! the table for a key space that saw one burst and will not again means dropping every handle to
//! the registry (every clone, every `Claim` still outstanding) so the backing allocation is freed
//! with it. A registry parked in a `LazyLock` static cannot be replaced this way; a static is for
//! a registry whose peak allocation is acceptable to hold for the life of the process.

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
    /// not go on to do the work turns away the caller that would have. Bind the returned claim
    /// and move it into the work; dropping it early defeats the deduplication silently, see the
    /// [module docs](self).
    ///
    /// The key is taken by value, so the losing path (the common one, since most callers past a
    /// refresh threshold get `None`) allocates an owned key and drops it. That is deliberate:
    /// the insert has to be atomic with the decision, and the obvious way to avoid the
    /// allocation, testing [`is_claimed`](ClaimRegistry::is_claimed) first and claiming only
    /// when it says no, is a time-of-check-to-time-of-use race that hands two callers the work.
    /// Pay the allocation.
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
    #[must_use]
    pub fn is_claimed<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.keys.lock().contains(key)
    }

    /// Returns the number of live claims.
    ///
    /// The count is a snapshot: another thread can claim or release a key before the caller acts
    /// on it. It is meant for tests asserting that the registry drains, and for diagnostics, not
    /// for deciding whether to do work.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.lock().len()
    }

    /// Returns `true` when no claim is live.
    ///
    /// Like [`len`](ClaimRegistry::len), the answer is a snapshot: another thread can claim a
    /// key the instant after it is read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.lock().is_empty()
    }
}

impl<K: fmt::Debug + Clone> fmt::Debug for ClaimRegistry<K> {
    /// Renders the claimed keys.
    ///
    /// The set is cloned under the lock (`HashSet::clone`, which does run `K::clone` while the
    /// lock is held) and formatted after the guard is dropped. What that buys is narrower than
    /// "no user code runs under the lock": `K::fmt` and the writer (the `stdout` lock behind a
    /// `println!`, say) are guaranteed not to run while the registry mutex is held. Holding the
    /// lock across the format itself, rather than just the clone, would make an ordinary log line
    /// deadlock against a concurrent claim or release, since [`claim`](ClaimRegistry::claim) and
    /// [`Claim`]'s `Drop` also take this same lock (running `K::hash`/`K::eq` while they hold it).
    ///
    /// This does not save a `K` whose own `Clone` impl re-enters the registry (calls
    /// [`claim`](ClaimRegistry::claim), [`is_claimed`](ClaimRegistry::is_claimed), or drops a
    /// [`Claim`] on it): `parking_lot::Mutex` is not reentrant, so that still deadlocks here, just
    /// as it would in `claim` or `Drop`. The `ReentrantKey` test below only re-enters through
    /// `K::fmt`, which runs after the lock is released; the `Clone`-reentrancy case is untested.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let claimed = self.keys.lock().clone();
        f.debug_struct("ClaimRegistry")
            .field("claimed", &claimed)
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
///
/// The `K: Eq + Hash` bounds sit on the struct, unlike [`ClaimRegistry`], which is bound-free
/// with the bounds on its impl blocks. They cannot be moved: the release is `Drop::drop` calling
/// `HashSet::remove`, and a `Drop` impl may not require bounds the struct itself does not declare
/// (`E0367`).
#[must_use = "a dropped claim releases the key immediately; bind it for the whole refresh"]
pub struct Claim<K: Eq + Hash> {
    registry: ClaimRegistry<K>,
    key: K,
}

impl<K: Eq + Hash> Claim<K> {
    /// Returns the claimed key.
    ///
    /// Passing this borrow to the work keeps the claim alive for the duration of the call the
    /// borrow is passed into: the guard cannot be dropped while that borrow is live. It does not
    /// pin the claim to the whole refresh; cloning the key out and dropping the claim releases
    /// it early, which is one of the anti-patterns in the [module docs](self).
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

    use std::sync::OnceLock;
    use std::sync::mpsc;
    use std::time::Duration;

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

    /// The registry a [`ReentrantKey`] reads back from while it is being formatted.
    static REENTRANT_REGISTRY: OnceLock<ClaimRegistry<ReentrantKey>> = OnceLock::new();

    /// A key whose `Debug` reads the registry that holds it, which is what an ordinary
    /// `println!` of a registry does from a second thread: run other code while the registry is
    /// being formatted. `parking_lot::Mutex` is not reentrant and cannot time out, so this
    /// wedges forever if `Debug for ClaimRegistry` holds the registry lock across the format.
    #[derive(Clone, PartialEq, Eq, Hash)]
    struct ReentrantKey(u32);

    impl fmt::Debug for ReentrantKey {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let live = REENTRANT_REGISTRY
                .get()
                .expect("registry installed before formatting")
                .len();
            write!(f, "ReentrantKey({}, live={live})", self.0)
        }
    }

    /// Receives the spawned thread's result, distinguishing the two `RecvTimeoutError` variants:
    /// a timeout means the registry mutex is genuinely wedged, while a disconnect means the
    /// sender died first, for a reason that has nothing to do with the mutex. Collapsing both
    /// into one `.expect(...)` reports an unrelated panic as a deadlock and sends the reader
    /// after the wrong bug. Shared with the test below, which is what pins this handling.
    fn recv_or_panic<T>(rx: &mpsc::Receiver<T>) -> T {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(sent) => sent,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("formatting the registry deadlocked on the registry mutex")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "the spawned thread panicked before sending, for a reason unrelated to the \
                     registry mutex (see the panic message printed above)"
                )
            }
        }
    }

    #[test]
    fn formatting_the_registry_does_not_hold_the_registry_lock() {
        // Every touch of the registry mutex, the claim included, happens on the spawned thread:
        // a regression then fails this test on the timeout below rather than wedging the test
        // binary, since the asserting thread never takes the lock (not even on its unwind).
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let registry = REENTRANT_REGISTRY.get_or_init(ClaimRegistry::new);
            let claim = registry.claim(ReentrantKey(1)).expect("first claim wins");
            let rendered = format!("{registry:?}");
            drop(claim);
            let _ = tx.send((rendered, registry.is_empty()));
        });

        let (rendered, drained) = recv_or_panic(&rx);
        assert!(rendered.contains("ReentrantKey(1"), "{rendered}");
        assert!(
            rendered.contains("live=1"),
            "the key set stays readable mid-format: {rendered}"
        );
        assert!(drained, "the claim released after the format");
    }

    #[test]
    fn default_is_an_empty_registry() {
        let registry: ClaimRegistry<u32> = ClaimRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    /// A regression test for `recv_or_panic`, the `recv_timeout` handling
    /// `formatting_the_registry_does_not_hold_the_registry_lock` receives through: an unrelated
    /// panic on the spawned thread (never touching the registry mutex at all) must be reported as
    /// a panic, not misattributed to a mutex deadlock. Before this was fixed, both
    /// `RecvTimeoutError` variants were collapsed by a single `.expect(...)`, so a panicked
    /// sender and an actual deadlock produced the identical "deadlocked on the registry mutex"
    /// message.
    #[test]
    fn recv_timeout_disconnected_is_reported_as_a_panic_not_a_deadlock() {
        // Deliberately does not touch the global panic hook: this test runs alongside every
        // other test in the binary, and `std::panic::set_hook` is process-global, so swapping it
        // here would race with unrelated tests' panics on other threads. The spawned thread's
        // panic message printing to stderr is expected output, not a failure.
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _tx = tx; // held so the channel disconnects only once this thread ends
            panic!("simulated unrelated failure, e.g. `expect(\"first claim wins\")`");
        });

        // Calls the same `recv_or_panic` the test above does, so reverting it to a single
        // `.expect(...)` fails here rather than passing against a private copy of the logic.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recv_or_panic(&rx)));

        let panic_payload = outcome.expect_err("recv_timeout on a disconnected channel panics");
        let message = panic_payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
            .expect("panic payload is a string");
        assert!(
            message.contains("panicked before sending"),
            "a disconnected channel must be reported as a panic, not a deadlock: {message}"
        );
        assert!(
            !message.contains("deadlocked"),
            "a disconnected channel must not be misreported as a mutex deadlock: {message}"
        );
    }
}
