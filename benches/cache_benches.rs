use cached::time::Duration;
use cached::{
    Cached, CachedRead, ConcurrentCached, Expires, ExpiringCache, ExpiringLruCache, LruCache,
    LruTtlCache, ShardedCache, ShardedLruCache, ShardedLruTtlCache, TtlCache, TtlSortedCache,
    UnboundCache,
};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
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

// ---- Group 1: unbounded cache -------------------------------------------------

fn bench_sharded_unbound_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("Concurrent Reads: ShardedCache vs single-lock");
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
    // serializes all writers.  ShardedCache avoids the single global lock entirely
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

    // ShardedCache: per-shard RwLocks eliminate inter-thread read contention.
    let sharded = ShardedCache::<usize, usize>::builder().build().unwrap();
    for i in 0..N_KEYS {
        sharded.cache_set(i, i * 2).expect("infallible");
    }
    group.bench_function("ShardedCache", |b| {
        b.iter_custom(|iters| {
            let cache = sharded.clone(); // Arc clone
            run_concurrent!(cache, iters, t, i, {
                black_box(cache.cache_get(&read_key(i, t)).expect("infallible"));
            })
        })
    });

    group.finish();

    // ---- Write benchmark (distinct keys, measures lock contention on inserts) ----
    let mut group = c.benchmark_group("Concurrent Writes: ShardedCache vs single-lock");
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

    let sharded_w = ShardedCache::<usize, usize>::builder().build().unwrap();
    group.bench_function("ShardedCache", |b| {
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

criterion_group!(
    benches,
    bench_cache_hits,
    bench_cache_misses_and_inserts,
    bench_eviction_overhead,
    bench_lock_synchronization,
    bench_sharded_unbound_concurrent,
    bench_sharded_lru_concurrent,
    bench_sharded_lru_ttl_concurrent,
);
criterion_main!(benches);
