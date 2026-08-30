//! Borrowed lookups at unsized `Q` types **other than `str` and `[u8]`**.
//!
//! `ShardHasher<K: ?Sized>` is what makes a borrowed lookup at an unsized key type route through
//! the trait at all. The tree's coverage of that relaxation is `str` (crate root, store modules,
//! `mod tests`) and `[u8]` (store modules, `tests/sharded_custom_router_lookups.rs`), which are
//! the two `Q`s the implementation was written against. Everything below is an unsized `Q` nobody
//! wrote the code with in mind:
//!
//! - `PathBuf` / `Path`, whose `Hash` is neither the `str` nor the `[u8]` impl;
//! - `OsString` / `OsStr`, likewise;
//! - `Vec<u32>` / `[u32]`, a slice of something other than bytes;
//! - `Arc<str>` / `str`, where the owned key type is itself a fat pointer rather than a `Vec`-like
//!   container;
//! - `Box<[u8]>` / `[u8]` through a **hand-written** two-impl router, which is the only place in
//!   the tree where a hand-written `impl ShardHasher<Q>` at an unsized `Q` other than `str` drives
//!   a real store.
//!
//! Each case runs all six inherent lookups (`get`/`peek`/`contains`/`remove`/`remove_entry`/
//! `delete`), because the `H: ShardHasher<Q>` bound is written out once per method and a
//! `?Sized`-related regression could reach one method and not another. Every case above drives
//! `ShardedUnboundCache`, one of three `HashMap`-backed sharded stores (with `ShardedTtlCache`
//! and `ShardedExpiringCache`): its `get`/`peek`/`contains` reach the key through `HashMap::get`,
//! a different probe path than the three LRU-backed sharded stores (`ShardedLruCache`,
//! `ShardedLruTtlCache`, `ShardedExpiringLruCache`), which reach it through
//! `LruCache::get_if`/`pop_raw`/`cache_peek` instead.
//! `path_keys_are_reachable_through_borrowed_path_lookups_on_an_lru_backed_store` below repeats
//! the `PathBuf`/`Path` case against `ShardedLruCache` so that probe path is covered by an
//! exotic `Q` too, rather than resting entirely on the in-module `&str`/`&[u8]` tests each store
//! module carries. The other four stores (`ShardedTtlCache`, `ShardedExpiringCache`,
//! `ShardedLruTtlCache`, `ShardedExpiringLruCache`) are not separately covered here; their
//! exotic-`Q` coverage rests on their own in-module tests.

use std::borrow::Borrow;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cached::{DefaultShardHasher, ShardHasher, ShardedLruCache, ShardedUnboundCache};

const SHARDS: usize = 8;
const KEYS: usize = 64;

/// The blanket impl must produce the same routing hash for an owned key and its unsized borrowed
/// form, for every one of these types. This is the `ShardHasher` half of the property; the store
/// tests below are the end-to-end half.
#[test]
fn the_blanket_impl_agrees_between_owned_and_unsized_borrowed_forms() {
    let h = DefaultShardHasher::new();

    let path = PathBuf::from("/var/tmp/key-7");
    assert_eq!(
        ShardHasher::<PathBuf>::shard_hash(&h, &path),
        ShardHasher::<Path>::shard_hash(&h, path.as_path()),
        "`PathBuf` and `&Path` must route identically"
    );

    let os: OsString = OsString::from("key-7");
    assert_eq!(
        ShardHasher::<OsString>::shard_hash(&h, &os),
        ShardHasher::<OsStr>::shard_hash(&h, os.as_os_str()),
        "`OsString` and `&OsStr` must route identically"
    );

    let nums: Vec<u32> = vec![7, 11, 13];
    assert_eq!(
        ShardHasher::<Vec<u32>>::shard_hash(&h, &nums),
        ShardHasher::<[u32]>::shard_hash(&h, nums.as_slice()),
        "`Vec<u32>` and `&[u32]` must route identically"
    );

    let shared: Arc<str> = Arc::from("key-7");
    assert_eq!(
        ShardHasher::<Arc<str>>::shard_hash(&h, &shared),
        ShardHasher::<str>::shard_hash(&h, &shared),
        "`Arc<str>` and `&str` must route identically"
    );

    let boxed: Box<[u8]> = b"key-7".to_vec().into_boxed_slice();
    assert_eq!(
        ShardHasher::<Box<[u8]>>::shard_hash(&h, &boxed),
        ShardHasher::<[u8]>::shard_hash(&h, &boxed),
        "`Box<[u8]>` and `&[u8]` must route identically"
    );

    // Distinct keys must not all collapse onto one value, or the assertions above would hold
    // vacuously for a degenerate hasher.
    assert_ne!(
        ShardHasher::<Path>::shard_hash(&h, path.as_path()),
        ShardHasher::<Path>::shard_hash(&h, Path::new("/var/tmp/key-8"))
    );
}

/// All six inherent lookups at one unsized `Q`, driven from a store keyed by the owned form.
///
/// `owned` builds key number `i`; `borrow` projects an owned key to the borrowed form the lookups
/// use. Written as one function so every key type below is held to the same list rather than to
/// whatever subset its own test happened to spell out.
fn exercise_six_lookups<K, Q>(
    label: &str,
    owned: impl Fn(usize) -> K,
    borrow: impl for<'a> Fn(&'a K) -> &'a Q,
) where
    K: std::hash::Hash + Eq + Clone + std::fmt::Debug + Borrow<Q>,
    Q: std::hash::Hash + Eq + ?Sized,
{
    let c = ShardedUnboundCache::<K, usize>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    let keys: Vec<K> = (0..KEYS).map(&owned).collect();
    for (i, k) in keys.iter().enumerate() {
        c.set(k.clone(), i);
    }
    assert_eq!(c.len(), KEYS, "{label}: every owned insert must be stored");
    let sizes = c.shard_sizes();
    assert!(
        sizes.iter().filter(|n| **n > 0).count() > 1,
        "{label}: a borrowed-routing check is only meaningful across several shards, saw {sizes:?}"
    );

    for (i, k) in keys.iter().enumerate() {
        let q = borrow(k);
        assert_eq!(c.get(q), Some(i), "{label}: borrowed `get` missed `{k:?}`");
        assert_eq!(
            c.peek(q),
            Some(i),
            "{label}: borrowed `peek` missed `{k:?}`"
        );
        assert!(c.contains(q), "{label}: borrowed `contains` missed `{k:?}`");
    }

    // Each removing method gets its own third of the key space.
    for (i, k) in keys.iter().enumerate() {
        let q = borrow(k);
        match i % 3 {
            0 => assert_eq!(
                c.remove(q),
                Some(i),
                "{label}: borrowed `remove` missed `{k:?}`"
            ),
            1 => assert_eq!(
                c.remove_entry(q),
                Some((k.clone(), i)),
                "{label}: borrowed `remove_entry` must hand back the stored owned key for `{k:?}`"
            ),
            _ => assert!(c.delete(q), "{label}: borrowed `delete` missed `{k:?}`"),
        }
        assert!(
            !c.contains(q),
            "{label}: `{k:?}` must be gone after its removal"
        );
    }
    assert!(
        c.is_empty(),
        "{label}: every entry must have been removed through the borrowed form"
    );
}

#[test]
fn path_keys_are_reachable_through_borrowed_path_lookups() {
    exercise_six_lookups::<PathBuf, Path>(
        "PathBuf/&Path",
        |i| PathBuf::from(format!("/var/tmp/key-{i}")),
        |k| k.as_path(),
    );
}

/// The same `PathBuf`/`Path` case as
/// [`path_keys_are_reachable_through_borrowed_path_lookups`], but against `ShardedLruCache`
/// instead of `ShardedUnboundCache`. `ShardedLruCache` is `LruCache`-backed: its `get`/`peek`/
/// `contains`/`remove_entry` reach the key through `LruCache::get_if`/`pop_raw`/`cache_peek`
/// (see `src/stores/sharded/lru.rs`), a different probe path than the `HashMap::get` every other
/// case in this file drives. Written out separately rather than folded into
/// `exercise_six_lookups`, which is generic over the store's owned/borrowed key types but not
/// over which store it builds.
#[test]
fn path_keys_are_reachable_through_borrowed_path_lookups_on_an_lru_backed_store() {
    let c = ShardedLruCache::<PathBuf, usize>::builder()
        .shards(SHARDS)
        .max_size(KEYS * 4)
        .build()
        .unwrap();
    let keys: Vec<PathBuf> = (0..KEYS)
        .map(|i| PathBuf::from(format!("/var/tmp/key-{i}")))
        .collect();
    for (i, k) in keys.iter().enumerate() {
        c.set(k.clone(), i);
    }
    assert_eq!(
        c.len(),
        KEYS,
        "every owned insert must be stored (no evictions expected at this capacity)"
    );
    let sizes = c.shard_sizes();
    assert!(
        sizes.iter().filter(|n| **n > 0).count() > 1,
        "a borrowed-routing check is only meaningful across several shards, saw {sizes:?}"
    );

    for (i, k) in keys.iter().enumerate() {
        let q: &Path = k.as_path();
        assert_eq!(c.get(q), Some(i), "borrowed `get` missed `{k:?}`");
        assert_eq!(c.peek(q), Some(i), "borrowed `peek` missed `{k:?}`");
        assert!(c.contains(q), "borrowed `contains` missed `{k:?}`");
    }

    for (i, k) in keys.iter().enumerate() {
        let q: &Path = k.as_path();
        match i % 3 {
            0 => assert_eq!(c.remove(q), Some(i), "borrowed `remove` missed `{k:?}`"),
            1 => assert_eq!(
                c.remove_entry(q),
                Some((k.clone(), i)),
                "borrowed `remove_entry` must hand back the stored owned key for `{k:?}`"
            ),
            _ => assert!(c.delete(q), "borrowed `delete` missed `{k:?}`"),
        }
        assert!(!c.contains(q), "`{k:?}` must be gone after its removal");
    }
    assert!(
        c.is_empty(),
        "every entry must have been removed through the borrowed form"
    );
}

#[test]
fn os_string_keys_are_reachable_through_borrowed_os_str_lookups() {
    exercise_six_lookups::<OsString, OsStr>(
        "OsString/&OsStr",
        |i| OsString::from(format!("key-{i}")),
        |k| k.as_os_str(),
    );
}

#[test]
fn u32_vec_keys_are_reachable_through_borrowed_slice_lookups() {
    exercise_six_lookups::<Vec<u32>, [u32]>(
        "Vec<u32>/&[u32]",
        |i| vec![i as u32, (i * 7) as u32, 0xdead_beef],
        |k| k.as_slice(),
    );
}

#[test]
fn arc_str_keys_are_reachable_through_borrowed_str_lookups() {
    exercise_six_lookups::<Arc<str>, str>(
        "Arc<str>/&str",
        |i| Arc::from(format!("key-{i}").as_str()),
        |k| k,
    );
}

/// The degenerate unsized key: length zero. An empty `Vec<u32>` / `Box<[u8]>` / `Arc<str>` hashes
/// to whatever the empty-sequence hash is, and its borrowed form has to route there too -- a
/// borrowed lookup that special-cased a null or dangling data pointer for an empty slice, or a
/// routing path that read the key's length before its bytes, would show up here and nowhere else.
/// The empty key is also inserted alongside a non-empty one so the assertions cannot pass on an
/// empty store.
#[test]
fn empty_unsized_keys_route_like_any_other() {
    let bytes = ShardedUnboundCache::<Box<[u8]>, u32>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    let empty_bytes: Box<[u8]> = Vec::new().into_boxed_slice();
    bytes.set(empty_bytes.clone(), 1);
    bytes.set(b"x".to_vec().into_boxed_slice(), 2);
    assert_eq!(
        bytes.get(b"".as_slice()),
        Some(1),
        "an empty `&[u8]` must reach the empty `Box<[u8]>` key"
    );
    assert!(bytes.contains(b"".as_slice()));
    assert_eq!(bytes.peek(b"".as_slice()), Some(1));
    assert_eq!(
        bytes.remove_entry(b"".as_slice()),
        Some((empty_bytes, 1)),
        "the stored empty key must come back out"
    );
    assert_eq!(bytes.len(), 1, "only the empty key was removed");

    let nums = ShardedUnboundCache::<Vec<u32>, u32>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    nums.set(Vec::new(), 1);
    nums.set(vec![9u32], 2);
    let empty_slice: &[u32] = &[];
    assert_eq!(
        nums.get(empty_slice),
        Some(1),
        "an empty `&[u32]` must reach the empty `Vec<u32>` key"
    );
    assert!(nums.delete(empty_slice));
    assert_eq!(nums.len(), 1);

    let text = ShardedUnboundCache::<Arc<str>, u32>::builder()
        .shards(SHARDS)
        .build()
        .unwrap();
    let empty_arc: Arc<str> = Arc::from("");
    text.set(empty_arc, 1);
    text.set(Arc::from("x"), 2);
    assert_eq!(
        text.get(""),
        Some(1),
        "an empty `&str` must reach the empty `Arc<str>` key"
    );
    assert_eq!(text.remove(""), Some(1));
    assert_eq!(text.len(), 1);
}

/// A hand-written router carrying an impl at an **unsized** key type. `ShardHasher<[u8]>` is only
/// writable at all because the trait parameter is `?Sized`; before that relaxation a router could
/// not opt into byte-slice lookups no matter what it implemented.
#[derive(Clone)]
struct ByteSliceRouter;

/// FNV-1a over the bytes, so both impls below demonstrably agree by construction: they run the
/// same function on the same bytes.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

impl ShardHasher<Box<[u8]>> for ByteSliceRouter {
    fn shard_hash(&self, key: &Box<[u8]>) -> u64 {
        fnv1a(key)
    }
}

impl ShardHasher<[u8]> for ByteSliceRouter {
    fn shard_hash(&self, key: &[u8]) -> u64 {
        fnv1a(key)
    }
}

#[test]
fn a_hand_written_router_can_opt_into_unsized_byte_slice_lookups() {
    let c = ShardedUnboundCache::<Box<[u8]>, usize>::builder()
        .shards(SHARDS)
        .hasher(ByteSliceRouter)
        .build()
        .unwrap();
    let keys: Vec<Box<[u8]>> = (0..KEYS)
        .map(|i| format!("key-{i}").into_bytes().into_boxed_slice())
        .collect();
    for (i, k) in keys.iter().enumerate() {
        c.set(k.clone(), i);
    }
    let sizes = c.shard_sizes();
    assert!(
        sizes.iter().filter(|n| **n > 0).count() > 1,
        "the router must spread these keys over several shards, saw {sizes:?}"
    );

    for (i, k) in keys.iter().enumerate() {
        let q: &[u8] = k;
        assert_eq!(c.get(q), Some(i), "borrowed `get` missed `{k:?}`");
        assert_eq!(c.peek(q), Some(i), "borrowed `peek` missed `{k:?}`");
        assert!(c.contains(q), "borrowed `contains` missed `{k:?}`");
    }
    assert_eq!(c.get(b"absent".as_slice()), None);

    for (i, k) in keys.iter().enumerate() {
        let q: &[u8] = k;
        match i % 3 {
            0 => assert_eq!(c.remove(q), Some(i)),
            1 => assert_eq!(c.remove_entry(q), Some((k.clone(), i))),
            _ => assert!(c.delete(q)),
        }
    }
    assert!(
        c.is_empty(),
        "every entry must have been removed through the unsized borrowed form"
    );
}
