use cached::time::Duration;
use cached::{
    Cached, CachedIter, CachedRead, ConcurrentCached, Expires, ExpiringCache, ExpiringLruCache,
    LruCache, LruTtlCache, ShardedExpiringCache, ShardedExpiringLruCache, ShardedLruCache,
    ShardedLruTtlCache, ShardedTtlCache, ShardedUnboundCache, TtlCache, TtlSortedCache,
    UnboundCache,
};
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

#[derive(Clone)]
#[allow(dead_code)]
struct ExpiringValue {
    val: usize,
}

impl Expires for ExpiringValue {
    fn is_expired(&self) -> bool {
        false
    }
}

/// Value type for the sweep/storm benchmarks, which need to control expiry directly
/// (as opposed to `ExpiringValue`, which is always live) without sleeping past a real
/// deadline in every setup closure.
#[derive(Clone)]
struct SweepExpiring {
    #[allow(dead_code)]
    val: usize,
    expired: bool,
}

impl Expires for SweepExpiring {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

/// `String` key generator for the destructive-sweep benchmarks: a real heap allocation
/// plus a re-hash on every lookup/removal, which a `usize` key can't expose.
#[inline]
fn skey(i: usize) -> String {
    format!("sweep-key-{i:08}")
}

fn bench_cache_hits(c: &mut Criterion) {
    let mut group = c.benchmark_group("Cache Hits (O(1) Reads)");
    let limit = 1000;
    let query_key = 500;

    // 1. UnboundCache
    let mut unbound = UnboundCache::builder().build().unwrap();
    for i in 0..limit {
        unbound.cache_set(i, i * 2);
    }
    group.bench_function("UnboundCache hit", |b| {
        b.iter(|| {
            let res = unbound.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // 2. LruCache
    let mut lru = LruCache::builder().max_size(limit).build().unwrap();
    for i in 0..limit {
        lru.cache_set(i, i * 2);
    }
    group.bench_function("LruCache hit", |b| {
        b.iter(|| {
            let res = lru.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // 3. TtlCache
    let mut ttl_cache = TtlCache::builder()
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    for i in 0..limit {
        ttl_cache.cache_set(i, i * 2);
    }
    group.bench_function("TtlCache hit (O(1))", |b| {
        b.iter(|| {
            let res = ttl_cache.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // 4. LruTtlCache
    let mut lru_ttl_cache = LruTtlCache::builder()
        .max_size(limit)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    for i in 0..limit {
        lru_ttl_cache.cache_set(i, i * 2);
    }
    group.bench_function("LruTtlCache hit (O(1))", |b| {
        b.iter(|| {
            let res = lru_ttl_cache.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // 5. ExpiringLruCache
    let mut expiring_lru_cache = ExpiringLruCache::builder().max_size(limit).build().unwrap();
    for i in 0..limit {
        expiring_lru_cache.cache_set(i, ExpiringValue { val: i * 2 });
    }
    group.bench_function("ExpiringLruCache hit (O(1))", |b| {
        b.iter(|| {
            let res = expiring_lru_cache.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // 6. TtlSortedCache
    let mut ttl_sorted_cache = TtlSortedCache::builder()
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    for i in 0..limit {
        let _ = ttl_sorted_cache.cache_set(i, i * 2);
    }
    group.bench_function("TtlSortedCache hit", |b| {
        b.iter(|| {
            let res = ttl_sorted_cache.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // 7. ExpiringCache
    let mut expiring_cache = ExpiringCache::builder().build().unwrap();
    for i in 0..limit {
        expiring_cache.cache_set(i, ExpiringValue { val: i * 2 });
    }
    group.bench_function("ExpiringCache hit (O(1))", |b| {
        b.iter(|| {
            let res = expiring_cache.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    group.finish();
}

fn bench_cache_misses_and_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("Cache Misses & Inserts");

    // Benchmark raw insertion without size limits/eviction
    group.bench_function("UnboundCache insert", |b| {
        let mut cache = UnboundCache::builder().build().unwrap();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("LruCache insert (no eviction)", |b| {
        let mut cache = LruCache::builder().max_size(100_000).build().unwrap();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("TtlCache insert (no eviction)", |b| {
        let mut cache = TtlCache::builder()
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("LruTtlCache insert (no eviction)", |b| {
        let mut cache = LruTtlCache::builder()
            .max_size(100_000)
            .ttl(Duration::from_secs(3600))
            .build()
            .unwrap();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("ExpiringCache insert", |b| {
        let mut cache: ExpiringCache<usize, ExpiringValue> =
            ExpiringCache::builder().build().unwrap();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, ExpiringValue { val: key * 2 });
            key += 1;
        })
    });

    group.finish();
}

fn bench_eviction_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("Eviction & Capacity Limits");
    let capacity = 1000;

    // LRU Cache constantly evicting (inserting into full cache)
    let mut lru = LruCache::builder().max_size(capacity).build().unwrap();
    for i in 0..capacity {
        lru.cache_set(i, i * 2);
    }
    let mut key = capacity;
    group.bench_function("LruCache eviction overhead", |b| {
        b.iter(|| {
            lru.cache_set(key, key * 2);
            key += 1;
        })
    });

    // LruTtl Cache constantly evicting
    let mut lru_ttl = LruTtlCache::builder()
        .max_size(capacity)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    for i in 0..capacity {
        lru_ttl.cache_set(i, i * 2);
    }
    let mut key = capacity;
    group.bench_function("LruTtlCache eviction overhead", |b| {
        b.iter(|| {
            lru_ttl.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.finish();
}

fn bench_lock_synchronization(c: &mut Criterion) {
    let mut group = c.benchmark_group("Lock Contention & Synchronization");
    let limit = 1000;
    let query_key = 500;

    // Simulate standard RwLock wrapping UnboundCache
    let unbound_lock = Arc::new(RwLock::new({
        let mut cache = UnboundCache::builder().build().unwrap();
        for i in 0..limit {
            cache.cache_set(i, i * 2);
        }
        cache
    }));

    // Standard write lock hit path
    let unbound_lock_clone = unbound_lock.clone();
    group.bench_function("RwLock UnboundCache write lock read", |b| {
        b.iter(|| {
            let mut cache = unbound_lock_clone.write();
            let res = cache.cache_get(black_box(&query_key));
            black_box(res);
        })
    });

    // Unsynchronized read path (using CachedRead trait)
    let unbound_lock_clone = unbound_lock.clone();
    group.bench_function("RwLock UnboundCache unsync read", |b| {
        b.iter(|| {
            let cache = unbound_lock_clone.read();
            let res = cache.cache_get_read(black_box(&query_key));
            black_box(res);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Concurrent benchmarks: sharded stores vs. single-lock equivalents
//
// Each group runs N_THREADS threads concurrently (barrier-synchronized) and
// reports combined throughput so the comparison is apples-to-apples.
//
// Throughput is set to N_THREADS elements per iteration: with iter_custom,
// each thread does `iters` ops so total ops = N_THREADS * iters.  Returning
// wall-clock elapsed (not summed CPU time) and setting throughput(N_THREADS)
// makes Criterion report the aggregate concurrent ops/sec for the whole pool.
// ---------------------------------------------------------------------------

const N_THREADS: usize = 4;
const N_KEYS: usize = 1_000;

/// Scattered read key: distributes thread-local sequential accesses across the
/// key space so that adjacent iterations don't alias in cache lines.
#[inline(always)]
fn read_key(i: usize, thread_id: usize) -> usize {
    (i.wrapping_mul(7).wrapping_add(thread_id.wrapping_mul(53))) % N_KEYS
}

/// Write key: each thread owns a distinct slice of the key space so writes on
/// different threads never contend on the same logical entry.  The single-lock
/// baselines still serialize all writes through one lock, but at the same
/// logical write rate; the sharded stores can serve them in parallel.
#[inline(always)]
fn write_key(i: usize, thread_id: usize) -> usize {
    let stride = N_KEYS / N_THREADS;
    thread_id * stride + (i % stride)
}

macro_rules! run_concurrent {
    ($cache:ident, $iters:expr, $thread_id:ident, $idx:ident, $bench_fn:block) => {{
        let ready_barrier = Arc::new(Barrier::new(N_THREADS + 1));
        let start_barrier = Arc::new(Barrier::new(N_THREADS + 1));
        let handles: Vec<_> = (0..N_THREADS)
            .map(|t| {
                let ready_barrier = ready_barrier.clone();
                let start_barrier = start_barrier.clone();
                let $cache = $cache.clone();
                thread::spawn(move || {
                    ready_barrier.wait();
                    start_barrier.wait();
                    let $thread_id = t;
                    let iters = $iters as usize;
                    for $idx in 0..iters {
                        $bench_fn
                    }
                })
            })
            .collect();
        ready_barrier.wait();
        let start = Instant::now();
        start_barrier.wait();
        for h in handles {
            h.join().expect("bench thread panicked");
        }
        start.elapsed()
    }};
}

/// Thread count for the expiry-storm and whole-cache-poll groups: wider than the
/// 4-thread groups above to stress the single global `evictions` counter harder.
const N_THREADS_STORM: usize = 8;

/// Entry count for the O(n) sweep benchmarks (evict/retain/retain_latest/key_order/
/// iter_order/`CachedIter::iter`).
const N_SWEEP: usize = 10_000;

/// Same shape as `run_concurrent!`, but with an explicit thread count instead of the
/// fixed `N_THREADS` constant, for the 8-thread expiry-storm and poll groups.
macro_rules! run_concurrent_n {
    ($n_threads:expr, $cache:ident, $iters:expr, $thread_id:ident, $idx:ident, $bench_fn:block) => {{
        let n_threads = $n_threads;
        let ready_barrier = Arc::new(Barrier::new(n_threads + 1));
        let start_barrier = Arc::new(Barrier::new(n_threads + 1));
        let handles: Vec<_> = (0..n_threads)
            .map(|t| {
                let ready_barrier = ready_barrier.clone();
                let start_barrier = start_barrier.clone();
                let $cache = $cache.clone();
                thread::spawn(move || {
                    ready_barrier.wait();
                    start_barrier.wait();
                    let $thread_id = t;
                    let iters = $iters as usize;
                    for $idx in 0..iters {
                        $bench_fn
                    }
                })
            })
            .collect();
        ready_barrier.wait();
        let start = Instant::now();
        start_barrier.wait();
        for h in handles {
            h.join().expect("bench thread panicked");
        }
        start.elapsed()
    }};
}

// ---- Group 1: unbounded cache -------------------------------------------------

fn bench_sharded_unbound_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("Concurrent Reads: ShardedUnboundCache vs single-lock");
    group.throughput(Throughput::Elements(N_THREADS as u64));

    // Baseline A: Mutex<HashMap> — every read takes an exclusive lock.
    let mutex_map: Arc<Mutex<HashMap<usize, usize>>> =
        Arc::new(Mutex::new((0..N_KEYS).map(|i| (i, i * 2)).collect()));
    group.bench_function("Mutex<HashMap>", |b| {
        b.iter_custom(|iters| {
            let map = mutex_map.clone();
            run_concurrent!(map, iters, t, i, {
                black_box(map.lock().get(&read_key(i, t)).copied());
            })
        })
    });

    // Baseline B: RwLock<HashMap> — readers share the lock, writers exclude.
    let rw_map: Arc<RwLock<HashMap<usize, usize>>> =
        Arc::new(RwLock::new((0..N_KEYS).map(|i| (i, i * 2)).collect()));
    group.bench_function("RwLock<HashMap>", |b| {
        b.iter_custom(|iters| {
            let map = rw_map.clone();
            run_concurrent!(map, iters, t, i, {
                black_box(map.read().get(&read_key(i, t)).copied());
            })
        })
    });

    // Baseline C: RwLock<UnboundCache> using CachedRead (shared read lock).
    // UnboundCache uses StripedCounter (16-slot padded atomics) for hits/misses
    // to reduce false sharing on the counter words, but the global RwLock still
    // serializes all writers.  ShardedUnboundCache avoids the single global lock entirely
    // by keeping both the lock and the counters per-shard.
    let rw_unbound = Arc::new(RwLock::new({
        let mut c = UnboundCache::builder().build().unwrap();
        for i in 0..N_KEYS {
            c.cache_set(i, i * 2usize);
        }
        c
    }));
    group.bench_function("RwLock<UnboundCache> (CachedRead)", |b| {
        b.iter_custom(|iters| {
            let cache = rw_unbound.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.read().cache_get_read(&read_key(i, t)));
            })
        })
    });

    // ShardedUnboundCache: per-shard RwLocks eliminate inter-thread read contention.
    let sharded = ShardedUnboundCache::<usize, usize>::builder()
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        sharded.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedUnboundCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded.clone(); // Arc clone
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();

    // ---- Write benchmark (distinct keys, measures lock contention on inserts) ----
    let mut group = c.benchmark_group("Concurrent Writes: ShardedUnboundCache vs single-lock");
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_map_w: Arc<Mutex<HashMap<usize, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    group.bench_function("Mutex<HashMap>", |b| {
        b.iter_custom(|iters| {
            let map = mutex_map_w.clone();
            run_concurrent!(map, iters, t, i, {
                map.lock().insert(write_key(i, t), i * 2);
            })
        })
    });

    let sharded_w = ShardedUnboundCache::<usize, usize>::builder()
        .build()
        .unwrap();
    group.bench_function("ShardedUnboundCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.cache_set(write_key(i, t), i * 2).expect("infallible");
            })
        })
    });

    group.finish();
}

// ---- Group 2: LRU cache -------------------------------------------------------
//
// LruCache::cache_get updates recency so it needs &mut self — every read must
// take an exclusive lock.  ShardedLruCache distributes that across shards.

fn bench_sharded_lru_concurrent(c: &mut Criterion) {
    let cap = 4 * N_KEYS; // large enough that eviction doesn't happen during reads

    let mut group = c.benchmark_group("Concurrent Reads: ShardedLruCache vs Mutex<LruCache>");
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_lru: Arc<Mutex<LruCache<usize, usize>>> = Arc::new(Mutex::new(
        LruCache::builder().max_size(cap).build().unwrap(),
    ));
    {
        let mut g = mutex_lru.lock();
        for i in 0..N_KEYS {
            g.cache_set(i, i * 2);
        }
    }
    group.bench_function("Mutex<LruCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_lru.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.lock().cache_get(&read_key(i, t)));
            })
        })
    });

    let sharded_lru = ShardedLruCache::<usize, usize>::builder()
        .max_size(cap)
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        sharded_lru.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedLruCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_lru.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();

    // ---- Write benchmark ------------------------------------------------------
    let mut group = c.benchmark_group("Concurrent Writes: ShardedLruCache vs Mutex<LruCache>");
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_lru_w: Arc<Mutex<LruCache<usize, usize>>> = Arc::new(Mutex::new(
        LruCache::builder().max_size(cap).build().unwrap(),
    ));
    group.bench_function("Mutex<LruCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_lru_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.lock().cache_set(write_key(i, t), i * 2);
            })
        })
    });

    let sharded_lru_w = ShardedLruCache::<usize, usize>::builder()
        .max_size(cap)
        .build()
        .unwrap();
    group.bench_function("ShardedLruCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_lru_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.cache_set(write_key(i, t), i * 2).expect("infallible");
            })
        })
    });

    group.finish();
}

// ---- Group 3: LRU + TTL -------------------------------------------------------

fn bench_sharded_lru_ttl_concurrent(c: &mut Criterion) {
    let cap = 4 * N_KEYS;
    let long_ttl = Duration::from_secs(3600);

    let mut group = c.benchmark_group("Concurrent Reads: ShardedLruTtlCache vs Mutex<LruTtlCache>");
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_lru_ttl: Arc<Mutex<LruTtlCache<usize, usize>>> = Arc::new(Mutex::new(
        LruTtlCache::builder()
            .max_size(cap)
            .ttl(long_ttl)
            .build()
            .unwrap(),
    ));
    {
        let mut g = mutex_lru_ttl.lock();
        for i in 0..N_KEYS {
            g.cache_set(i, i * 2);
        }
    }
    group.bench_function("Mutex<LruTtlCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_lru_ttl.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.lock().cache_get(&read_key(i, t)));
            })
        })
    });

    let sharded_lru_ttl = ShardedLruTtlCache::<usize, usize>::builder()
        .max_size(cap)
        .ttl(long_ttl)
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        sharded_lru_ttl.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedLruTtlCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_lru_ttl.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();

    // ---- Write benchmark ------------------------------------------------------
    let mut group =
        c.benchmark_group("Concurrent Writes: ShardedLruTtlCache vs Mutex<LruTtlCache>");
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_lru_ttl_w: Arc<Mutex<LruTtlCache<usize, usize>>> = Arc::new(Mutex::new(
        LruTtlCache::builder()
            .max_size(cap)
            .ttl(long_ttl)
            .build()
            .unwrap(),
    ));
    group.bench_function("Mutex<LruTtlCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_lru_ttl_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.lock().cache_set(write_key(i, t), i * 2);
            })
        })
    });

    let sharded_lru_ttl_w = ShardedLruTtlCache::<usize, usize>::builder()
        .max_size(cap)
        .ttl(long_ttl)
        .build()
        .unwrap();
    group.bench_function("ShardedLruTtlCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_lru_ttl_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.cache_set(write_key(i, t), i * 2).expect("infallible");
            })
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Sweeps at N=10,000: evict/retain/retain_latest/key_order/iter_order/
// CachedIter::iter. These are O(n) paths that walk every entry, sample the clock
// per entry (evict/retain on the TTL-aware stores), and -- on the destructive ops --
// remove doomed entries, cloning their keys along the way.
//
// evict/retain/retain_latest are destructive (they drain the cache), so each is
// rebuilt from scratch per-iteration via `iter_batched`: the setup closure's cost is
// excluded from the measurement, but it does mean the *wall time* of these functions
// is dominated by the N_SWEEP-entry rebuild, not just the swept op -- hence the very
// short measurement/warm-up windows below.
//
// LruCache has no TTL/expiry concept, so it has no `evict()`. TtlCache has no LRU
// ordering, so it has no `key_order`/`iter_order`.
// ---------------------------------------------------------------------------

fn bench_sweeps(c: &mut Criterion) {
    let mut group = c.benchmark_group("Sweeps (evict/retain/order) at N=10,000");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));
    group.throughput(Throughput::Elements(N_SWEEP as u64));

    // ==== TtlCache ===========================================================

    // evict(): ttl=1ns means every entry is already expired by the time the
    // N_SWEEP-entry setup loop finishes, so evict() removes all of them.
    group.bench_function("TtlCache evict (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlCache::builder()
                    .ttl(Duration::from_nanos(1))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlCache evict (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlCache::builder()
                    .ttl(Duration::from_nanos(1))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });

    // retain(): a long ttl means nothing auto-expires, so the sweep cost measured
    // here is purely the predicate-driven removal (and doomed-key handling) of half
    // the entries.
    group.bench_function("TtlCache retain (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlCache::builder()
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| cache.retain(|k, _v| *k % 2 == 0),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlCache retain (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlCache::builder()
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| cache.retain(|_k, v| *v % 2 == 0),
            BatchSize::SmallInput,
        )
    });

    // CachedIter::iter(): non-destructive, so one cache serves every sample.
    let mut ttl_iter_cache = TtlCache::builder()
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    for i in 0..N_SWEEP {
        ttl_iter_cache.cache_set(i, i);
    }
    group.bench_function("TtlCache CachedIter::iter (usize keys)", |b| {
        b.iter(|| black_box(ttl_iter_cache.iter().count()))
    });

    // ==== LruTtlCache ========================================================

    group.bench_function("LruTtlCache evict (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = LruTtlCache::builder()
                    .max_size(N_SWEEP)
                    .ttl(Duration::from_nanos(1))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("LruTtlCache evict (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = LruTtlCache::builder()
                    .max_size(N_SWEEP)
                    .ttl(Duration::from_nanos(1))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("LruTtlCache retain (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = LruTtlCache::builder()
                    .max_size(N_SWEEP)
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| cache.retain(|k, _v| *k % 2 == 0),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("LruTtlCache retain (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = LruTtlCache::builder()
                    .max_size(N_SWEEP)
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| cache.retain(|_k, v| *v % 2 == 0),
            BatchSize::SmallInput,
        )
    });

    let mut lru_ttl_order_cache = LruTtlCache::builder()
        .max_size(N_SWEEP)
        .ttl(Duration::from_secs(3600))
        .build()
        .unwrap();
    for i in 0..N_SWEEP {
        lru_ttl_order_cache.cache_set(i, i);
    }
    group.bench_function("LruTtlCache key_order (usize keys)", |b| {
        b.iter(|| black_box(lru_ttl_order_cache.key_order()))
    });
    group.bench_function("LruTtlCache iter_order (usize keys)", |b| {
        b.iter(|| black_box(lru_ttl_order_cache.iter_order()))
    });
    group.bench_function("LruTtlCache CachedIter::iter (usize keys)", |b| {
        b.iter(|| black_box(lru_ttl_order_cache.iter().count()))
    });

    // ==== LruCache (no TTL, so no evict()) ==================================

    group.bench_function("LruCache retain (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = LruCache::builder().max_size(N_SWEEP).build().unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| cache.retain(|k, _v| *k % 2 == 0),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("LruCache retain (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = LruCache::builder().max_size(N_SWEEP).build().unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| cache.retain(|_k, v| *v % 2 == 0),
            BatchSize::SmallInput,
        )
    });

    let mut lru_order_cache = LruCache::builder().max_size(N_SWEEP).build().unwrap();
    for i in 0..N_SWEEP {
        lru_order_cache.cache_set(i, i);
    }
    group.bench_function("LruCache key_order (usize keys)", |b| {
        b.iter(|| black_box(lru_order_cache.key_order()))
    });
    group.bench_function("LruCache iter_order (usize keys)", |b| {
        b.iter(|| black_box(lru_order_cache.iter_order()))
    });
    group.bench_function("LruCache CachedIter::iter (usize keys)", |b| {
        b.iter(|| black_box(lru_order_cache.iter().count()))
    });

    // ==== ExpiringLruCache ===================================================

    group.bench_function("ExpiringLruCache evict (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = ExpiringLruCache::builder()
                    .max_size(N_SWEEP)
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(
                        i,
                        SweepExpiring {
                            val: i,
                            expired: true,
                        },
                    );
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("ExpiringLruCache evict (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = ExpiringLruCache::builder()
                    .max_size(N_SWEEP)
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(
                        skey(i),
                        SweepExpiring {
                            val: i,
                            expired: true,
                        },
                    );
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("ExpiringLruCache retain (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = ExpiringLruCache::builder()
                    .max_size(N_SWEEP)
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(
                        i,
                        SweepExpiring {
                            val: i,
                            expired: false,
                        },
                    );
                }
                cache
            },
            |mut cache| cache.retain(|k, _v| *k % 2 == 0),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("ExpiringLruCache retain (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = ExpiringLruCache::builder()
                    .max_size(N_SWEEP)
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    cache.cache_set(
                        skey(i),
                        SweepExpiring {
                            val: i,
                            expired: false,
                        },
                    );
                }
                cache
            },
            |mut cache| cache.retain(|_k, v| v.val % 2 == 0),
            BatchSize::SmallInput,
        )
    });

    let mut elru_order_cache = ExpiringLruCache::builder()
        .max_size(N_SWEEP)
        .build()
        .unwrap();
    for i in 0..N_SWEEP {
        elru_order_cache.cache_set(
            i,
            SweepExpiring {
                val: i,
                expired: false,
            },
        );
    }
    group.bench_function("ExpiringLruCache key_order (usize keys)", |b| {
        b.iter(|| black_box(elru_order_cache.key_order()))
    });
    group.bench_function("ExpiringLruCache iter_order (usize keys)", |b| {
        b.iter(|| black_box(elru_order_cache.iter_order()))
    });
    group.bench_function("ExpiringLruCache CachedIter::iter (usize keys)", |b| {
        b.iter(|| black_box(elru_order_cache.iter().count()))
    });

    // ==== TtlSortedCache =====================================================

    group.bench_function("TtlSortedCache evict (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlSortedCache::builder()
                    .ttl(Duration::from_nanos(1))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    let _ = cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlSortedCache evict (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlSortedCache::builder()
                    .ttl(Duration::from_nanos(1))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    let _ = cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| black_box(cache.evict()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlSortedCache retain (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlSortedCache::builder()
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    let _ = cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| cache.retain(|k, _v| *k % 2 == 0),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlSortedCache retain (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlSortedCache::builder()
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    let _ = cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| cache.retain(|_k, v| *v % 2 == 0),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlSortedCache retain_latest (usize keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlSortedCache::builder()
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    let _ = cache.cache_set(i, i);
                }
                cache
            },
            |mut cache| black_box(cache.retain_latest(N_SWEEP / 2, false)),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("TtlSortedCache retain_latest (String keys)", |b| {
        b.iter_batched(
            || {
                let mut cache = TtlSortedCache::builder()
                    .ttl(Duration::from_secs(3600))
                    .build()
                    .unwrap();
                for i in 0..N_SWEEP {
                    let _ = cache.cache_set(skey(i), i);
                }
                cache
            },
            |mut cache| black_box(cache.retain_latest(N_SWEEP / 2, false)),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// refresh_on_hit hit path: TtlCache and LruTtlCache, refresh on vs off, so the
// per-hit cost of re-sampling the clock and rewriting expires_at is visible as a
// delta between the two.
// ---------------------------------------------------------------------------

fn bench_refresh_on_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("TTL refresh_on_hit: hit path delta");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));

    let limit = 1000;
    let query_key = 500;
    let long_ttl = Duration::from_secs(3600);

    for refresh in [false, true] {
        let mut cache = TtlCache::builder()
            .ttl(long_ttl)
            .refresh_on_hit(refresh)
            .build()
            .unwrap();
        for i in 0..limit {
            cache.cache_set(i, i * 2);
        }
        group.bench_function(format!("TtlCache hit (refresh_on_hit={refresh})"), |b| {
            b.iter(|| {
                black_box(cache.cache_get(black_box(&query_key)));
            })
        });
    }

    for refresh in [false, true] {
        let mut cache = LruTtlCache::builder()
            .max_size(limit)
            .ttl(long_ttl)
            .refresh_on_hit(refresh)
            .build()
            .unwrap();
        for i in 0..limit {
            cache.cache_set(i, i * 2);
        }
        group.bench_function(format!("LruTtlCache hit (refresh_on_hit={refresh})"), |b| {
            b.iter(|| {
                black_box(cache.cache_get(black_box(&query_key)));
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Overwrite path: cache_set over an EXISTING key, with and without an on_evict
// callback configured. The callback only fires when the displaced value had already
// expired, but implementations may still pay for a key clone (or other setup) on every
// call whenever a callback is *configured*, whether or not it ends up firing -- this
// group makes that delta visible.
// ---------------------------------------------------------------------------

fn bench_overwrite_existing_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("Overwrite: cache_set on an existing (live) key");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));

    let limit = 1000;
    let key = 500;
    let long_ttl = Duration::from_secs(3600);

    // TtlCache: on_evict() returns `Self`, so the callback can be attached conditionally.
    for with_callback in [false, true] {
        let mut builder = TtlCache::<usize, usize>::builder().ttl(long_ttl);
        if with_callback {
            builder = builder.on_evict(|_k: &usize, _v: &usize| {});
        }
        let mut cache = builder.build().unwrap();
        for i in 0..limit {
            cache.cache_set(i, i);
        }
        let mut val = 0usize;
        group.bench_function(
            format!("TtlCache cache_set overwrite (on_evict={with_callback})"),
            |b| {
                b.iter(|| {
                    cache.cache_set(black_box(key), black_box(val));
                    val = val.wrapping_add(1);
                })
            },
        );
    }

    // LruTtlCache: on_evict() is a typestate transition (NoEvict -> HasEvict), so the
    // two configurations need separate builder chains rather than a runtime branch.
    {
        let mut cache = LruTtlCache::<usize, usize>::builder()
            .max_size(limit)
            .ttl(long_ttl)
            .build()
            .unwrap();
        for i in 0..limit {
            cache.cache_set(i, i);
        }
        let mut val = 0usize;
        group.bench_function("LruTtlCache cache_set overwrite (on_evict=false)", |b| {
            b.iter(|| {
                cache.cache_set(black_box(key), black_box(val));
                val = val.wrapping_add(1);
            })
        });
    }
    {
        let mut cache = LruTtlCache::<usize, usize>::builder()
            .max_size(limit)
            .ttl(long_ttl)
            .on_evict(|_k: &usize, _v: &usize| {})
            .build()
            .unwrap();
        for i in 0..limit {
            cache.cache_set(i, i);
        }
        let mut val = 0usize;
        group.bench_function("LruTtlCache cache_set overwrite (on_evict=true)", |b| {
            b.iter(|| {
                cache.cache_set(black_box(key), black_box(val));
                val = val.wrapping_add(1);
            })
        });
    }

    // TtlSortedCache
    for with_callback in [false, true] {
        let mut builder = TtlSortedCache::<usize, usize>::builder().ttl(long_ttl);
        if with_callback {
            builder = builder.on_evict(|_k: &usize, _v: &usize| {});
        }
        let mut cache = builder.build().unwrap();
        for i in 0..limit {
            let _ = cache.cache_set(i, i);
        }
        let mut val = 0usize;
        group.bench_function(
            format!("TtlSortedCache cache_set overwrite (on_evict={with_callback})"),
            |b| {
                b.iter(|| {
                    let _ = cache.cache_set(black_box(key), black_box(val));
                    val = val.wrapping_add(1);
                })
            },
        );
    }

    // ExpiringLruCache: cache_set clones the key up front whenever a callback is
    // configured (to have it ready if the displaced value turns out to be expired),
    // even though this overwrite is always of a live entry and the callback never
    // fires -- this is exactly the wasted-clone cost this group is meant to expose.
    for with_callback in [false, true] {
        let mut builder = ExpiringLruCache::<usize, SweepExpiring>::builder().max_size(limit);
        if with_callback {
            builder = builder.on_evict(|_k: &usize, _v: &SweepExpiring| {});
        }
        let mut cache = builder.build().unwrap();
        for i in 0..limit {
            cache.cache_set(
                i,
                SweepExpiring {
                    val: i,
                    expired: false,
                },
            );
        }
        let mut val = 0usize;
        group.bench_function(
            format!("ExpiringLruCache cache_set overwrite (on_evict={with_callback})"),
            |b| {
                b.iter(|| {
                    cache.cache_set(
                        black_box(key),
                        SweepExpiring {
                            val: black_box(val),
                            expired: false,
                        },
                    );
                    val = val.wrapping_add(1);
                })
            },
        );
    }

    // Same overwrite, but with a `String` key: `usize` is `Copy`, so the unconditional
    // `k.clone()` above is invisible in the usize benchmark above -- a `String` clone is
    // a real heap allocation and should make the on_evict=true delta visible.
    for with_callback in [false, true] {
        let skey_fixed = skey(key);
        let mut builder = ExpiringLruCache::<String, SweepExpiring>::builder().max_size(limit);
        if with_callback {
            builder = builder.on_evict(|_k: &String, _v: &SweepExpiring| {});
        }
        let mut cache = builder.build().unwrap();
        for i in 0..limit {
            cache.cache_set(
                skey(i),
                SweepExpiring {
                    val: i,
                    expired: false,
                },
            );
        }
        let mut val = 0usize;
        group.bench_function(
            format!(
                "ExpiringLruCache cache_set overwrite (on_evict={with_callback}) (String keys)"
            ),
            |b| {
                b.iter(|| {
                    cache.cache_set(
                        black_box(skey_fixed.clone()),
                        SweepExpiring {
                            val: black_box(val),
                            expired: false,
                        },
                    );
                    val = val.wrapping_add(1);
                })
            },
        );
    }

    // TtlSortedCache::set_and_get_mut via cache_get_or_set_with_mut on a miss (the
    // `#[cached]` write path): every key here is new, so this always takes the
    // miss -> set_and_get_mut branch.
    {
        let mut cache = TtlSortedCache::<usize, usize>::builder()
            .ttl(long_ttl)
            .build()
            .unwrap();
        let mut key_ctr = 0usize;
        group.bench_function(
            "TtlSortedCache cache_get_or_set_with_mut (miss -> set_and_get_mut)",
            |b| {
                b.iter(|| {
                    let v = cache.cache_get_or_set_with_mut(key_ctr, || key_ctr);
                    black_box(v);
                    key_ctr += 1;
                })
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Whole-cache ops: cache_clear_with_on_evict (with/without callback, LRU family),
// len(), and metrics() on a 256-shard cache.
// ---------------------------------------------------------------------------

fn bench_whole_cache_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("Whole-cache ops: clear/len/metrics");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));

    let limit = 1000;
    let long_ttl = Duration::from_secs(3600);

    // LruCache
    for with_callback in [false, true] {
        group.bench_function(
            format!("LruCache cache_clear_with_on_evict (on_evict={with_callback})"),
            |b| {
                b.iter_batched(
                    || {
                        let mut builder = LruCache::<usize, usize>::builder().max_size(limit);
                        if with_callback {
                            builder = builder.on_evict(|_k: &usize, _v: &usize| {});
                        }
                        let mut cache = builder.build().unwrap();
                        for i in 0..limit {
                            cache.cache_set(i, i);
                        }
                        cache
                    },
                    |mut cache| cache.cache_clear_with_on_evict(),
                    BatchSize::SmallInput,
                )
            },
        );
    }

    // LruTtlCache: on_evict() typestate again means two explicit blocks.
    group.bench_function(
        "LruTtlCache cache_clear_with_on_evict (on_evict=false)",
        |b| {
            b.iter_batched(
                || {
                    let mut cache = LruTtlCache::<usize, usize>::builder()
                        .max_size(limit)
                        .ttl(long_ttl)
                        .build()
                        .unwrap();
                    for i in 0..limit {
                        cache.cache_set(i, i);
                    }
                    cache
                },
                |mut cache| cache.cache_clear_with_on_evict(),
                BatchSize::SmallInput,
            )
        },
    );
    group.bench_function(
        "LruTtlCache cache_clear_with_on_evict (on_evict=true)",
        |b| {
            b.iter_batched(
                || {
                    let mut cache = LruTtlCache::<usize, usize>::builder()
                        .max_size(limit)
                        .ttl(long_ttl)
                        .on_evict(|_k: &usize, _v: &usize| {})
                        .build()
                        .unwrap();
                    for i in 0..limit {
                        cache.cache_set(i, i);
                    }
                    cache
                },
                |mut cache| cache.cache_clear_with_on_evict(),
                BatchSize::SmallInput,
            )
        },
    );

    // ExpiringLruCache
    for with_callback in [false, true] {
        group.bench_function(
            format!("ExpiringLruCache cache_clear_with_on_evict (on_evict={with_callback})"),
            |b| {
                b.iter_batched(
                    || {
                        let mut builder =
                            ExpiringLruCache::<usize, SweepExpiring>::builder().max_size(limit);
                        if with_callback {
                            builder = builder.on_evict(|_k: &usize, _v: &SweepExpiring| {});
                        }
                        let mut cache = builder.build().unwrap();
                        for i in 0..limit {
                            cache.cache_set(
                                i,
                                SweepExpiring {
                                    val: i,
                                    expired: false,
                                },
                            );
                        }
                        cache
                    },
                    |mut cache| cache.cache_clear_with_on_evict(),
                    BatchSize::SmallInput,
                )
            },
        );
    }

    // len()/metrics() on a 256-shard cache: both are O(shards) fan-outs, so a large
    // shard count makes their cost visible.
    let big_shard_cache = ShardedUnboundCache::<usize, usize>::builder()
        .shards(256)
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        big_shard_cache.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedUnboundCache len() (256 shards)", |b| {
        b.iter(|| black_box(big_shard_cache.len()))
    });
    group.bench_function("ShardedUnboundCache metrics() (256 shards)", |b| {
        b.iter(|| black_box(big_shard_cache.metrics()))
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Sharded single-threaded per-op cache_get hit for all six sharded stores. Only a
// 4-thread aggregate exists elsewhere in this file, which hides per-op regressions
// (e.g. a single-shard lock/hash/clone cost that gets amortized away in the aggregate).
// ---------------------------------------------------------------------------

fn bench_sharded_single_threaded_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("Sharded stores: single-threaded cache_get hit");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));

    let limit = N_KEYS;
    let query_key = N_KEYS / 2;
    let long_ttl = Duration::from_secs(3600);

    let unbound = ShardedUnboundCache::<usize, usize>::builder()
        .build()
        .unwrap();
    for i in 0..limit {
        unbound.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedUnboundCache", |b| {
        b.iter(|| {
            black_box(
                unbound
                    .cache_get(black_box(&query_key))
                    .expect("infallible"),
            )
        })
    });

    let lru = ShardedLruCache::<usize, usize>::builder()
        .max_size(limit)
        .build()
        .unwrap();
    for i in 0..limit {
        lru.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedLruCache", |b| {
        b.iter(|| black_box(lru.cache_get(black_box(&query_key)).expect("infallible")))
    });

    let lru_ttl = ShardedLruTtlCache::<usize, usize>::builder()
        .max_size(limit)
        .ttl(long_ttl)
        .build()
        .unwrap();
    for i in 0..limit {
        lru_ttl.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedLruTtlCache", |b| {
        b.iter(|| {
            black_box(
                lru_ttl
                    .cache_get(black_box(&query_key))
                    .expect("infallible"),
            )
        })
    });

    let ttl = ShardedTtlCache::<usize, usize>::builder()
        .ttl(long_ttl)
        .build()
        .unwrap();
    for i in 0..limit {
        ttl.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedTtlCache", |b| {
        b.iter(|| black_box(ttl.cache_get(black_box(&query_key)).expect("infallible")))
    });

    let expiring = ShardedExpiringCache::<usize, ExpiringValue>::builder()
        .build()
        .unwrap();
    for i in 0..limit {
        expiring
            .cache_set(i, ExpiringValue { val: i })
            .expect("infallible");
    }
    group.bench_function("ShardedExpiringCache", |b| {
        b.iter(|| {
            black_box(
                expiring
                    .cache_get(black_box(&query_key))
                    .expect("infallible"),
            )
        })
    });

    let expiring_lru = ShardedExpiringLruCache::<usize, ExpiringValue>::builder()
        .max_size(limit)
        .build()
        .unwrap();
    for i in 0..limit {
        expiring_lru
            .cache_set(i, ExpiringValue { val: i })
            .expect("infallible");
    }
    group.bench_function("ShardedExpiringLruCache", |b| {
        b.iter(|| {
            black_box(
                expiring_lru
                    .cache_get(black_box(&query_key))
                    .expect("infallible"),
            )
        })
    });

    group.finish();
}

// ---- Group 4: ExpiringLruCache -------------------------------------------------

fn bench_sharded_expiring_lru_concurrent(c: &mut Criterion) {
    let cap = 4 * N_KEYS;

    let mut group =
        c.benchmark_group("Concurrent Reads: ShardedExpiringLruCache vs Mutex<ExpiringLruCache>");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_elru: Arc<Mutex<ExpiringLruCache<usize, ExpiringValue>>> = Arc::new(Mutex::new(
        ExpiringLruCache::builder().max_size(cap).build().unwrap(),
    ));
    {
        let mut g = mutex_elru.lock();
        for i in 0..N_KEYS {
            g.cache_set(i, ExpiringValue { val: i });
        }
    }
    group.bench_function("Mutex<ExpiringLruCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_elru.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.lock().cache_get(&read_key(i, t)));
            })
        })
    });

    let sharded_elru = ShardedExpiringLruCache::<usize, ExpiringValue>::builder()
        .max_size(cap)
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        sharded_elru
            .cache_set(i, ExpiringValue { val: i })
            .expect("infallible");
    }
    group.bench_function("ShardedExpiringLruCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_elru.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();

    let mut group =
        c.benchmark_group("Concurrent Writes: ShardedExpiringLruCache vs Mutex<ExpiringLruCache>");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_elru_w: Arc<Mutex<ExpiringLruCache<usize, ExpiringValue>>> = Arc::new(Mutex::new(
        ExpiringLruCache::builder().max_size(cap).build().unwrap(),
    ));
    group.bench_function("Mutex<ExpiringLruCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_elru_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache
                    .lock()
                    .cache_set(write_key(i, t), ExpiringValue { val: i });
            })
        })
    });

    let sharded_elru_w = ShardedExpiringLruCache::<usize, ExpiringValue>::builder()
        .max_size(cap)
        .build()
        .unwrap();
    group.bench_function("ShardedExpiringLruCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_elru_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache
                    .cache_set(write_key(i, t), ExpiringValue { val: i })
                    .expect("infallible");
            })
        })
    });

    group.finish();
}

// ---- Group 5: TtlCache (unbounded, TTL only) -----------------------------------

fn bench_sharded_ttl_concurrent(c: &mut Criterion) {
    let long_ttl = Duration::from_secs(3600);

    let mut group = c.benchmark_group("Concurrent Reads: ShardedTtlCache vs Mutex<TtlCache>");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_ttl: Arc<Mutex<TtlCache<usize, usize>>> = Arc::new(Mutex::new(
        TtlCache::builder().ttl(long_ttl).build().unwrap(),
    ));
    {
        let mut g = mutex_ttl.lock();
        for i in 0..N_KEYS {
            g.cache_set(i, i * 2);
        }
    }
    group.bench_function("Mutex<TtlCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_ttl.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.lock().cache_get(&read_key(i, t)));
            })
        })
    });

    let sharded_ttl_no_refresh = ShardedTtlCache::<usize, usize>::builder()
        .ttl(long_ttl)
        .refresh_on_hit(false)
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        sharded_ttl_no_refresh
            .cache_set(i, i * 2)
            .expect("infallible");
    }
    group.bench_function("ShardedTtlCache (refresh_on_hit=false)", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_ttl_no_refresh.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    let sharded_ttl_refresh = ShardedTtlCache::<usize, usize>::builder()
        .ttl(long_ttl)
        .refresh_on_hit(true)
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        sharded_ttl_refresh.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedTtlCache (refresh_on_hit=true)", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_ttl_refresh.clone();
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();

    let mut group = c.benchmark_group("Concurrent Writes: ShardedTtlCache vs Mutex<TtlCache>");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(N_THREADS as u64));

    let mutex_ttl_w: Arc<Mutex<TtlCache<usize, usize>>> = Arc::new(Mutex::new(
        TtlCache::builder().ttl(long_ttl).build().unwrap(),
    ));
    group.bench_function("Mutex<TtlCache>", |b| {
        b.iter_custom(|iters| {
            let cache = mutex_ttl_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.lock().cache_set(write_key(i, t), i * 2);
            })
        })
    });

    let sharded_ttl_w = ShardedTtlCache::<usize, usize>::builder()
        .ttl(long_ttl)
        .build()
        .unwrap();
    group.bench_function("ShardedTtlCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded_ttl_w.clone();
            run_concurrent!(cache, iters, t, i, {
                cache.cache_set(write_key(i, t), i * 2).expect("infallible");
            })
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Expiry storm: 8 threads, every entry already expired, so every concurrent read
// takes the lazy-eviction path and contends on the single global `evictions`
// AtomicU64 (ShardedTtlCache / ShardedExpiringLruCache both use one counter shared
// across all shards, rather than a per-shard counter).
// ---------------------------------------------------------------------------

fn bench_expiry_storm(c: &mut Criterion) {
    let mut group = c.benchmark_group("Expiry storm: 8 threads, every read is a lazy eviction");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(N_THREADS_STORM as u64));

    // ttl=1ns: by the time the N_KEYS-entry population loop below finishes, every
    // entry is already expired.
    let storm_ttl = ShardedTtlCache::<usize, usize>::new(Duration::from_nanos(1));
    for i in 0..N_KEYS {
        storm_ttl.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedTtlCache (all entries expired)", |b| {
        b.iter_custom(|iters| {
            let cache = storm_ttl.clone();
            run_concurrent_n!(N_THREADS_STORM, cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    let storm_elru = ShardedExpiringLruCache::<usize, SweepExpiring>::new(N_KEYS);
    for i in 0..N_KEYS {
        storm_elru
            .cache_set(
                i,
                SweepExpiring {
                    val: i,
                    expired: true,
                },
            )
            .expect("infallible");
    }
    group.bench_function("ShardedExpiringLruCache (all entries expired)", |b| {
        b.iter_custom(|iters| {
            let cache = storm_elru.clone();
            run_concurrent_n!(N_THREADS_STORM, cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 8 threads polling len()/metrics() concurrently against a live, populated cache.
// ---------------------------------------------------------------------------

fn bench_sharded_poll(c: &mut Criterion) {
    let mut group = c.benchmark_group("8 threads polling len()/metrics() concurrently");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(500));
    group.throughput(Throughput::Elements(N_THREADS_STORM as u64));

    let poll_cache = ShardedUnboundCache::<usize, usize>::builder()
        .build()
        .unwrap();
    for i in 0..N_KEYS {
        poll_cache.cache_set(i, i * 2).expect("infallible");
    }

    group.bench_function("ShardedUnboundCache len()", |b| {
        b.iter_custom(|iters| {
            let cache = poll_cache.clone();
            run_concurrent_n!(N_THREADS_STORM, cache, iters, _t, _i, {
                black_box(cache.len());
            })
        })
    });

    group.bench_function("ShardedUnboundCache metrics()", |b| {
        b.iter_custom(|iters| {
            let cache = poll_cache.clone();
            run_concurrent_n!(N_THREADS_STORM, cache, iters, _t, _i, {
                black_box(cache.metrics());
            })
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Build time: builder().build() for each sharded store, at the default shard count.
// The default shard count calls `available_parallelism()` on every build, which
// parses /proc files on Linux.
// ---------------------------------------------------------------------------

fn bench_sharded_build_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("Sharded store build() time (default shard count)");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(50));
    group.measurement_time(Duration::from_millis(200));

    group.bench_function("ShardedUnboundCache::builder().build()", |b| {
        b.iter(|| {
            let cache = ShardedUnboundCache::<usize, usize>::builder()
                .build()
                .unwrap();
            black_box(cache);
        })
    });

    group.bench_function("ShardedLruCache::builder().build()", |b| {
        b.iter(|| {
            let cache = ShardedLruCache::<usize, usize>::builder()
                .max_size(1000)
                .build()
                .unwrap();
            black_box(cache);
        })
    });

    group.bench_function("ShardedLruTtlCache::builder().build()", |b| {
        b.iter(|| {
            let cache = ShardedLruTtlCache::<usize, usize>::builder()
                .max_size(1000)
                .ttl(Duration::from_secs(3600))
                .build()
                .unwrap();
            black_box(cache);
        })
    });

    group.bench_function("ShardedTtlCache::builder().build()", |b| {
        b.iter(|| {
            let cache = ShardedTtlCache::<usize, usize>::builder()
                .ttl(Duration::from_secs(3600))
                .build()
                .unwrap();
            black_box(cache);
        })
    });

    group.bench_function("ShardedExpiringCache::builder().build()", |b| {
        b.iter(|| {
            let cache = ShardedExpiringCache::<usize, ExpiringValue>::builder()
                .build()
                .unwrap();
            black_box(cache);
        })
    });

    group.bench_function("ShardedExpiringLruCache::builder().build()", |b| {
        b.iter(|| {
            let cache = ShardedExpiringLruCache::<usize, ExpiringValue>::builder()
                .max_size(1000)
                .build()
                .unwrap();
            black_box(cache);
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Large-V LRU: LruCache<usize, [u8; 512]> hit/insert/eviction. Decision instrument
// for a proposed LRU node-layout change -- this group exists so that change can be
// judged; it does not itself change any store code.
// ---------------------------------------------------------------------------

fn bench_large_value_lru(c: &mut Criterion) {
    let mut group = c.benchmark_group("LruCache<usize, [u8; 512]>: large-value node layout");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(300));

    let capacity = 1000;

    let mut lru: LruCache<usize, [u8; 512]> =
        LruCache::builder().max_size(capacity).build().unwrap();
    for i in 0..capacity {
        lru.cache_set(i, [i as u8; 512]);
    }
    let query_key = capacity / 2;
    group.bench_function("hit", |b| {
        b.iter(|| {
            black_box(lru.cache_get(black_box(&query_key)));
        })
    });

    group.bench_function("insert (no eviction)", |b| {
        let mut cache: LruCache<usize, [u8; 512]> =
            LruCache::builder().max_size(100_000).build().unwrap();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, [key as u8; 512]);
            key += 1;
        })
    });

    let mut lru_full: LruCache<usize, [u8; 512]> =
        LruCache::builder().max_size(capacity).build().unwrap();
    for i in 0..capacity {
        lru_full.cache_set(i, [i as u8; 512]);
    }
    let mut key = capacity;
    group.bench_function("eviction overhead", |b| {
        b.iter(|| {
            lru_full.cache_set(key, [key as u8; 512]);
            key += 1;
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Clock cost: a standalone micro-benchmark of `Instant::now()` itself, so the clock
// cost is recorded in the baseline rather than being a remembered constant.
// ---------------------------------------------------------------------------

fn bench_instant_now(c: &mut Criterion) {
    let mut group = c.benchmark_group("Clock cost");
    group.sample_size(50);
    group.warm_up_time(Duration::from_millis(300));
    group.measurement_time(Duration::from_secs(1));

    group.bench_function("Instant::now()", |b| b.iter(|| black_box(Instant::now())));

    group.finish();
}

criterion_group!(
    benches,
    bench_cache_hits,
    bench_cache_misses_and_inserts,
    bench_eviction_overhead,
    bench_lock_synchronization,
    bench_sharded_unbound_concurrent,
    bench_sharded_lru_concurrent,
    bench_sharded_lru_ttl_concurrent,
    bench_sweeps,
    bench_refresh_on_hit,
    bench_overwrite_existing_key,
    bench_whole_cache_ops,
    bench_sharded_single_threaded_hit,
    bench_sharded_expiring_lru_concurrent,
    bench_sharded_ttl_concurrent,
    bench_expiry_storm,
    bench_sharded_poll,
    bench_sharded_build_time,
    bench_large_value_lru,
    bench_instant_now,
);
criterion_main!(benches);
