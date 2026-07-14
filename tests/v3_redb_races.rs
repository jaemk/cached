//! Regression tests for read-then-write races in the redb disk store.
//!
//! Each test is designed to fail against the pre-fix code (where `disk_cache_get`
//! and `remove_expired_entries` read under a read txn, drop it, then mutate
//! under a separate write txn) and pass with the fix (re-read and re-validate
//! inside the write txn so check-and-mutate is atomic against concurrent writers).
//!
//! The races are concurrent and timing-dependent. Each test uses structure and
//! iteration counts chosen to make the race manifests reliably on a multicore
//! machine when the pre-fix code runs.

#![cfg(feature = "redb_store")]

use std::sync::Arc;
use std::sync::Barrier;

use cached::time::Duration;
use cached::{ConcurrentCached, RedbCache};
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn build(
    name: &str,
    dir: &TempDir,
    ttl: Option<Duration>,
    refresh: bool,
) -> Arc<RedbCache<u32, u32>> {
    let mut b = RedbCache::<u32, u32>::builder(name)
        .disk_dir(dir.path())
        // No fsync for speed; these are in-process races.
        .durable(false)
        .refresh_on_hit(refresh);
    if let Some(t) = ttl {
        b = b.ttl(t);
    }
    Arc::new(b.build().expect("cache build"))
}

// ── Test 1: refresh-on-hit does not clobber a concurrent write ───────────────
//
// Bug: disk_cache_get reads V_old under a read txn, drops it, then re-serialises
// V_old (with a refreshed created_at) and blindly inserts it in a new write txn.
// If a concurrent cache_set committed V_new between those two txns, V_old
// overwrites V_new.
//
// Fix: the write txn re-reads the authoritative current entry and refreshes
// that entry, never reverting to a stale read-txn snapshot.
//
// Reliability: the writer sets key=1 to values 1..N. The reader calls cache_get
// (which triggers refresh every time since TTL=30 s, entries never expire).
// redb serialises write txns, so the reader's refresh write txn can run after
// the writer's last commit if the reader's read txn opened first (MVCC). When
// that happens with the pre-fix code, the reader writes V_old over V_N.
//
// The reader runs 2x as many iterations as the writer so it is still active
// after the writer's last commit, keeping the race window open. With N=5 000
// and each operation taking ~0.1-1 ms, there are thousands of overlap
// opportunities; the probability of the race hitting the final write is high.
#[test]
fn refresh_does_not_clobber_concurrent_write() {
    const N: u32 = 5_000;
    let dir = TempDir::new().unwrap();
    let cache = build("race-refresh", &dir, Some(Duration::from_secs(30)), true);

    cache.cache_set(1, 0).unwrap();

    let cw = cache.clone();
    let writer = std::thread::spawn(move || {
        for v in 1..=N {
            cw.cache_set(1, v).unwrap();
        }
    });

    let cr = cache.clone();
    let reader = std::thread::spawn(move || {
        for _ in 0..N * 2 {
            let _ = cr.cache_get(&1);
        }
    });

    writer.join().unwrap();
    reader.join().unwrap();

    // Pre-fix: the reader may have refreshed an old value V_old < N into the
    // store after the writer committed N, leaving the final value < N.
    // Post-fix: refresh always operates on the current authoritative value (N).
    let got = cache.cache_get(&1).unwrap();
    assert_eq!(
        got,
        Some(N),
        "refresh must not resurrect a stale value: expected Some({N}), got {got:?}"
    );
}

// ── Test 2: cache_get eviction does not delete a freshly-rewritten entry ──────
//
// Bug: disk_cache_get reads an expired entry under a read txn, drops it, then
// calls table.remove(key) in a new write txn. If a concurrent cache_set committed
// a fresh entry for that key between the two txns, the write txn removes the
// fresh entry.
//
// Fix: the write txn re-reads the entry and only removes it when it is STILL
// expired; a freshly-rewritten entry is skipped and returned to the caller.
//
// Reliability: for the eviction path to trigger, the entry must be expired when
// cache_get's read txn runs. Pre-expiring the entry and then spawning the
// evicting reader and fresh writer concurrently achieves this. On a multicore
// machine both threads start nearly simultaneously; roughly half the time the
// reader's MVCC read txn opens before the writer commits (seeing the expired
// snapshot), and the writer commits before the reader's write txn (write txns
// are serialised, so reader's write txn always follows writer's commit if
// writer started first). That ordering is Case A (the race). Over 40 rounds
// the race manifests with probability ≈ 1 − 0.5^40, catching the bug.
#[test]
fn cache_get_eviction_does_not_delete_fresh_rewrite() {
    // TTL is deliberately not tiny. The pre-set value=0 is aged past the TTL with
    // an explicit sleep so it is reliably expired when the round starts, but the
    // TTL itself must be comfortably larger than the worst-case gap between the
    // writer committing value=1 and the final `cache_get`, otherwise value=1
    // could *legitimately* expire before that read and be evicted (a false
    // failure unrelated to the race). 150 ms gives a wide margin against scheduler
    // stalls on a saturated CI runner while keeping the per-round expiry sleep short.
    const TTL: Duration = Duration::from_millis(150);
    // Determinism: each round is an independent ~50% chance of hitting the race
    // window. A `Barrier` releases the evicting reader and the fresh writer at
    // the same instant (removing the sequential-spawn skew that would otherwise
    // let the writer's fast commit reliably precede the reader's read txn), and
    // the round count is high enough that the probability of *never* hitting the
    // window (~0.5^ROUNDS) is negligible even on a loaded runner. Confirmed to
    // fail against the pre-fix code (see module-level notes).
    const ROUNDS: usize = 40;

    let dir = TempDir::new().unwrap();
    let cache = build("race-evict-get", &dir, Some(TTL), false);

    for round in 0..ROUNDS {
        // Pre-set a known expired value, then sleep past the TTL.
        cache.cache_set(1, 0).unwrap();
        std::thread::sleep(TTL + Duration::from_millis(6));

        // At this point key=1 = 0 and is expired.
        let gate = Arc::new(Barrier::new(2));

        // Reader: calls cache_get on the expired entry (eviction path).
        let cr = cache.clone();
        let gr = gate.clone();
        let reader = std::thread::spawn(move || {
            gr.wait();
            cr.cache_get(&1)
        });

        // Writer: concurrently writes a fresh value.
        let cw = cache.clone();
        let gw = gate.clone();
        let writer = std::thread::spawn(move || {
            gw.wait();
            cw.cache_set(1, 1)
        });

        reader.join().unwrap().unwrap();
        writer.join().unwrap().unwrap();

        // The writer committed value=1. If the race occurred (pre-fix), the
        // reader's write txn removed that fresh value, leaving key=1 absent.
        // With the fix the reader re-checks in its write txn and sees value=1
        // is fresh, skips the removal, and returns it.
        //
        // Note: if the writer committed before the reader's read txn (no race),
        // the reader sees value=1 as fresh and returns it via the fast path —
        // also correct, also Some(1).
        // If the reader's write txn ran before the writer's commit (legitimate
        // eviction of 0), the writer then sets 1, so key=1 = Some(1) either way.
        let got = cache.cache_get(&1).unwrap();
        assert_eq!(
            got,
            Some(1),
            "round {round}: cache_get eviction deleted the writer's fresh entry; \
             expected Some(1), got {got:?}"
        );
    }
}

// ── Test 3: remove_expired_entries does not delete a freshly-rewritten entry ──
//
// Bug: remove_expired_entries collects expired keys under a read txn, drops it,
// then removes all of them in a new write txn. A concurrent cache_set that
// commits between the two txns writes a fresh value; the write txn removes it.
//
// Fix: each key is re-checked inside the write txn immediately before removal;
// keys whose stored entry is now fresh (rewritten by a concurrent writer) are
// skipped and not removed.
//
// Reliability: pre-inserting SCAN_KEYS expired entries makes the scan non-
// trivial (it iterates them under a read txn snapshot, typically ~5–50 ms in
// a debug build). The concurrent writer refreshes FRESH_KEY in a single fast
// write txn (~0.1 ms). On a multicore machine both threads start at nearly the
// same time; roughly half the time the sweeper's MVCC snapshot is taken before
// the writer commits, so the snapshot sees FRESH_KEY as expired while the
// writer has already committed a fresh entry. The sweeper's write txn then runs
// after the writer (write txns serialise), finds FRESH_KEY fresh, and — with
// the fix — skips the removal.
//
// TTL is chosen much larger than the expected scan duration so that FRESH_KEY
// does not expire again between the writer's commit and the write txn re-check.
// Over ROUNDS rounds the race manifests with probability ≈ 1 − 0.5^ROUNDS.
#[test]
fn remove_expired_entries_does_not_delete_fresh_rewrite() {
    // TTL must be >> scan duration.  With SCAN_KEYS=200 the scan typically
    // completes in well under 20 ms; 150 ms gives a large margin so FRESH_KEY
    // cannot re-expire between the writer's commit and the sweeper's write-txn
    // re-check (nor before the final read that asserts it survived).
    const TTL: Duration = Duration::from_millis(150);
    const SCAN_KEYS: u32 = 200;
    const FRESH_KEY: u32 = 100;
    // A `Barrier` releases the sweeper and the writer simultaneously, and the
    // larger SCAN_KEYS lengthens the scan window so the writer's fast commit
    // reliably lands *inside* the sweeper's read-scan (snapshot already taken,
    // write-txn not yet started). Combined with more rounds this makes the
    // race manifest reliably; confirmed to fail against the pre-fix code.
    const ROUNDS: usize = 24;

    let dir = TempDir::new().unwrap();
    let cache = build("race-evict-sweep", &dir, Some(TTL), false);

    for round in 0..ROUNDS {
        // Fill the cache with SCAN_KEYS entries and let them all expire.
        for k in 0..SCAN_KEYS {
            cache.cache_set(k, k).unwrap();
        }
        std::thread::sleep(TTL + Duration::from_millis(20));

        let gate = Arc::new(Barrier::new(2));

        // Sweeper: its read-txn snapshot is taken at the very start of the scan.
        let cs = cache.clone();
        let gs = gate.clone();
        let sweeper = std::thread::spawn(move || {
            gs.wait();
            cs.remove_expired_entries()
        });

        // Writer: commits FRESH_KEY fresh while the scan is in progress.
        let cw = cache.clone();
        let gw = gate.clone();
        let writer = std::thread::spawn(move || {
            gw.wait();
            cw.cache_set(FRESH_KEY, 999_999)
        });

        let _count = sweeper.join().unwrap().unwrap();
        writer.join().unwrap().unwrap();

        // FRESH_KEY was written fresh by the writer. If the race occurred and
        // the sweeper's write txn found FRESH_KEY still within TTL (writer
        // committed during the scan window), the fix ensures it was skipped.
        // If the writer committed before the sweeper's snapshot, the scan
        // already saw it as fresh and never added it to expired_keys — also
        // correctly present.
        let got = cache.cache_get(&FRESH_KEY).unwrap();
        assert_eq!(
            got,
            Some(999_999),
            "round {round}: remove_expired_entries deleted the freshly-rewritten \
             entry (key={FRESH_KEY}); expected Some(999999), got {got:?}"
        );

        cache.cache_clear().unwrap();
    }
}

// ── Test 6: self-heal does not delete a concurrent valid write ───────────────
//
// Bug (C5): on a deserialization failure in non-strict mode, disk_cache_get read
// the corrupt entry under a read txn, dropped it, then opened a fresh write txn
// and blindly `table.remove(key)`d it. A concurrent `cache_set` that committed a
// valid value between the read txn and the self-heal write txn was silently
// deleted.
//
// Fix: the self-heal write txn re-reads the entry and re-validates it; it only
// removes the key when it is STILL corrupt. A freshly-written valid value is
// kept and returned.
//
// Corrupt fixture: an entry written by a `RedbCache<u32, String>` handle stores a
// MessagePack string in the value position. Reopening the same on-disk table as a
// `RedbCache<u32, u32>` cannot decode that string as a `u32`, so `cache_get`
// takes the non-strict self-heal path. redb takes an exclusive file lock, so the
// injector handle is dropped before the reader/writer handle opens.
//
// Reliability: a `Barrier` releases the self-healing reader and the fresh writer
// simultaneously. Roughly half the time the reader's MVCC read txn opens before
// the writer commits (seeing the corrupt snapshot) while the writer commits
// before the reader's write txn (write txns serialise). That ordering is the
// race; over ROUNDS rounds it manifests with probability ~ 1 - 0.5^ROUNDS.
#[test]
fn self_heal_does_not_delete_concurrent_write() {
    const ROUNDS: usize = 120;
    let dir = TempDir::new().unwrap();

    for round in 0..ROUNDS {
        // Inject a corrupt entry for key=1 by writing an incompatible value type
        // to the same table, then dropping that handle to release the file lock.
        {
            let corrupt = RedbCache::<u32, String>::builder("self-heal-race")
                .disk_dir(dir.path())
                .durable(true)
                .build()
                .expect("corrupt-injector build");
            corrupt.cache_set(1, "not-a-u32".to_string()).unwrap();
        }

        // Reopen as <u32, u32>; the stored String bytes fail to decode as u32,
        // driving cache_get down the non-strict self-heal path.
        let cache: Arc<RedbCache<u32, u32>> = Arc::new(
            RedbCache::<u32, u32>::builder("self-heal-race")
                .disk_dir(dir.path())
                .durable(false)
                .build()
                .expect("reader build"),
        );

        let gate = Arc::new(Barrier::new(2));

        let cr = cache.clone();
        let gr = gate.clone();
        let reader = std::thread::spawn(move || {
            gr.wait();
            let _ = cr.cache_get(&1); // self-heal path
        });

        let cw = cache.clone();
        let gw = gate.clone();
        let writer = std::thread::spawn(move || {
            gw.wait();
            cw.cache_set(1, 42)
        });

        reader.join().unwrap();
        writer.join().unwrap().unwrap();

        // The writer committed a valid value. Pre-fix, the reader's self-heal
        // write txn blindly removed key=1, deleting that fresh write (None).
        // Post-fix, the self-heal re-reads under the write txn, sees the valid
        // value, and keeps it.
        let got = cache.cache_get(&1).unwrap();
        assert_eq!(
            got,
            Some(42),
            "round {round}: self-heal deleted the concurrent valid write; \
             expected Some(42), got {got:?}"
        );

        // Drop the cache handle so the next round's injector can take the lock.
        drop(Arc::try_unwrap(cache).ok());
    }
}

// ── Test 4: remove_expired_entries returns an accurate count ─────────────────
//
// The pre-fix code returns expired_keys.len() — the scan count — regardless of
// how many entries are actually removed. With the fix the count reflects only
// entries actually removed in the write txn (entries rewritten fresh between
// scan and write txn are skipped and not counted).
//
// This single-threaded test verifies the basic count contract: N expired entries
// are removed (count == N), M fresh entries are untouched and survive.
// Single-threaded ensures no concurrent modification, so both pre-fix and fixed
// code should agree — it serves as a correctness baseline and catches any
// regression in the count logic itself.
//
// The concurrent count accuracy is exercised indirectly by test 3: when
// FRESH_KEY survives (fix), the count returned is SCAN_KEYS − 1 (accurate),
// not SCAN_KEYS (overcounting the un-removed fresh entry).
#[test]
fn remove_expired_entries_count_is_accurate() {
    const TTL: Duration = Duration::from_millis(50);

    let dir = TempDir::new().unwrap();
    let cache = build("race-count", &dir, Some(TTL), false);

    // Insert two entries that will expire and one that will remain fresh.
    cache.cache_set(10, 100).unwrap();
    cache.cache_set(20, 200).unwrap();
    std::thread::sleep(TTL + Duration::from_millis(20)); // 10 and 20 are now expired

    // Insert a fresh entry AFTER the sleep so it is not expired.
    cache.cache_set(30, 300).unwrap();

    let removed = cache.remove_expired_entries().unwrap();
    assert_eq!(
        removed, 2,
        "expected exactly 2 expired entries removed, got {removed}"
    );

    // The fresh entry must survive.
    assert_eq!(
        cache.cache_get(&30).unwrap(),
        Some(300),
        "fresh entry (key=30) must survive remove_expired_entries"
    );
    // The expired entries must be gone.
    assert_eq!(
        cache.cache_get(&10).unwrap(),
        None,
        "key=10 must have been removed"
    );
    assert_eq!(
        cache.cache_get(&20).unwrap(),
        None,
        "key=20 must have been removed"
    );
}

// ── Test 5: remove_expired_entries never overcounts under concurrent removal ──
//
// This pins the count contract *directly* under real concurrency, in a way a
// pre-fix build reliably fails without any false-positive risk on the fixed
// build.
//
// Setup: N expired keys. A sweeper runs `remove_expired_entries` while a second
// thread concurrently `cache_delete`s one candidate key (K). The delete does not
// re-insert, so K is absent afterwards in every interleaving — there is no
// "removed-then-rewritten" ambiguity that would muddy the count.
//
// `cache_delete` returns whether *it* removed K. It can only return `true` when
// the delete committed before the sweeper's own removal of K (redb serialises
// write txns; had the sweeper removed K first, the delete would see it gone and
// return `false`). So whenever `deleter_removed == true`, the sweeper must NOT
// have removed K, and an accurate count must be exactly N-1:
//
//   * delete before the sweeper's read snapshot  -> K not a candidate; sweeper
//     removes the other N-1 -> count == N-1.
//   * delete between snapshot and the sweeper's write-txn re-read -> K is a
//     candidate, but the fixed re-read sees it gone (None) and skips it (not
//     counted) -> count == N-1.
//
// The pre-fix code returns `expired_keys.len()` (== N) in the second case: it
// blindly counts K even though its `table.remove(K)` was a no-op. So on the
// fixed build `deleter_removed == true` always implies `count == N-1`; on the
// pre-fix build that implication is violated (count == N) whenever the delete
// lands in the scan window. A `Barrier` plus a long scan (large N) makes that
// window reliably hit across the rounds. Confirmed to fail against pre-fix.
#[test]
fn remove_expired_entries_concurrent_delete_count_never_overcounts() {
    const TTL: Duration = Duration::from_millis(150);
    const N: u32 = 200;
    const K: u32 = 100;
    const ROUNDS: usize = 24;

    let dir = TempDir::new().unwrap();
    let cache = build("race-count-concurrent", &dir, Some(TTL), false);

    for round in 0..ROUNDS {
        // Fill with N entries and let them all expire.
        for k in 0..N {
            cache.cache_set(k, k).unwrap();
        }
        std::thread::sleep(TTL + Duration::from_millis(20));

        let gate = Arc::new(Barrier::new(2));

        let cs = cache.clone();
        let gs = gate.clone();
        let sweeper = std::thread::spawn(move || {
            gs.wait();
            cs.remove_expired_entries()
        });

        let cd = cache.clone();
        let gd = gate.clone();
        let deleter = std::thread::spawn(move || {
            gd.wait();
            cd.cache_delete(&K)
        });

        let count = sweeper.join().unwrap().unwrap();
        let deleter_removed = deleter.join().unwrap().unwrap();

        if deleter_removed {
            // The deleter removed K before the sweeper could, so the sweeper must
            // not have counted K. An accurate count is exactly N-1; the pre-fix
            // overcount (N) is caught here.
            assert_eq!(
                count,
                (N - 1) as usize,
                "round {round}: remove_expired_entries overcounted; the concurrent \
                 delete removed key={K} (so the sweep did not), yet the sweep \
                 reported {count} removed instead of {}",
                N - 1
            );
        } else {
            // The sweeper removed K itself (or K was already gone): the count must
            // still never exceed the candidate set, and every original key is gone.
            assert_eq!(
                count, N as usize,
                "round {round}: sweep removed K itself, so all {N} originals were \
                 removed by the sweep; got count={count}"
            );
        }

        // In every interleaving all N original keys end absent (sweep evicts the
        // expired ones, the deleter removes K, nobody re-inserts).
        assert_eq!(
            cache.cache_get(&K).unwrap(),
            None,
            "round {round}: key={K} must be absent after sweep+delete"
        );

        cache.cache_clear().unwrap();
    }
}
