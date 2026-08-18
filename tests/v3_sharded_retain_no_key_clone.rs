//! `retain` on the `HashMap`-backed sharded stores must not require `K: Clone`.
//!
//! Making a panicking predicate safe means sweeping in two phases (select, then remove). The
//! obvious way to carry the selection across the phases is a `Vec<K>` of doomed keys, which
//! costs a `K::clone` plus a second hash and probe per removed entry, and forces `K: Clone` on
//! every caller. These stores instead record a `Vec<bool>` of decisions and replay it through
//! `extract_if`, which needs no bound at all (see `stores::take_doomed`).
//!
//! A test that merely calls `retain` cannot prove a bound is absent -- it would keep passing if
//! `K: Clone` were added back, since the key types in every other test happen to be `Clone`.
//! The proof here is [`NoCloneKey`], which does not implement `Clone`: re-adding the bound makes
//! this file fail to compile.

use cached::{Expires, ShardedExpiringCache, ShardedUnboundCache};

#[cfg(feature = "time_stores")]
use cached::ShardedTtlCache;

/// A key that is `Hash + Eq` but deliberately **not** `Clone`. The missing derive is the
/// assertion; do not add one.
#[derive(Debug, PartialEq, Eq, Hash)]
struct NoCloneKey(u32);

/// A value that never expires, so only the `keep` predicate decides.
#[derive(Clone, Debug, PartialEq)]
struct Live(u32);

impl Expires for Live {
    fn is_expired(&self) -> bool {
        false
    }
}

const ENTRIES: u32 = 16;
/// `retain` keeps the even keys, so it removes exactly half.
const EXPECTED_REMOVED: usize = (ENTRIES / 2) as usize;

#[test]
fn sharded_unbound_retain_accepts_a_non_clone_key() {
    let cache = ShardedUnboundCache::<NoCloneKey, u32>::builder()
        .build()
        .expect("build must succeed");
    for i in 0..ENTRIES {
        cache.set(NoCloneKey(i), i);
    }
    assert_eq!(cache.len(), ENTRIES as usize);

    let removed = cache.retain(|k, _v| k.0 % 2 == 0);

    assert_eq!(removed, EXPECTED_REMOVED, "half the keys are odd");
    assert_eq!(cache.len(), ENTRIES as usize - EXPECTED_REMOVED);
    assert!(cache.contains(&NoCloneKey(0)), "even keys are kept");
    assert!(!cache.contains(&NoCloneKey(1)), "odd keys are removed");
}

#[test]
fn sharded_expiring_retain_accepts_a_non_clone_key() {
    let cache = ShardedExpiringCache::<NoCloneKey, Live>::builder()
        .build()
        .expect("build must succeed");
    for i in 0..ENTRIES {
        cache.set(NoCloneKey(i), Live(i));
    }
    assert_eq!(cache.len(), ENTRIES as usize);

    let removed = cache.retain(|k, _v| k.0 % 2 == 0);

    assert_eq!(removed, EXPECTED_REMOVED, "half the keys are odd");
    assert_eq!(cache.len(), ENTRIES as usize - EXPECTED_REMOVED);
    assert!(cache.contains(&NoCloneKey(0)), "even keys are kept");
    assert!(!cache.contains(&NoCloneKey(1)), "odd keys are removed");
}

#[cfg(feature = "time_stores")]
#[test]
fn sharded_ttl_retain_accepts_a_non_clone_key() {
    let cache = ShardedTtlCache::<NoCloneKey, u32>::builder()
        .ttl(std::time::Duration::from_secs(3600))
        .build()
        .expect("build must succeed");
    for i in 0..ENTRIES {
        cache.set(NoCloneKey(i), i);
    }
    assert_eq!(cache.len(), ENTRIES as usize);

    let removed = cache.retain(|k, _v| k.0 % 2 == 0);

    assert_eq!(removed, EXPECTED_REMOVED, "half the keys are odd");
    assert_eq!(cache.len(), ENTRIES as usize - EXPECTED_REMOVED);
    assert!(cache.contains(&NoCloneKey(0)), "even keys are kept");
    assert!(!cache.contains(&NoCloneKey(1)), "odd keys are removed");
}
