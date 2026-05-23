use cached::time::Duration;
use cached::{
    Cached, CachedRead, Expires, ExpiringCache, ExpiringLruCache, LruCache, LruTtlCache, TtlCache,
    TtlSortedCache, UnboundCache,
};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use parking_lot::RwLock;
use std::sync::Arc;

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
    let mut unbound = UnboundCache::new();
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
    let mut lru = LruCache::with_size(limit);
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
    let mut ttl_cache = TtlCache::builder().ttl(Duration::from_secs(3600)).build();
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
        .size(limit)
        .ttl(Duration::from_secs(3600))
        .build();
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
    let mut expiring_lru_cache = ExpiringLruCache::builder().size(limit).build();
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
        .build();
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
    let mut expiring_cache = ExpiringCache::new();
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
        let mut cache = UnboundCache::new();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("LruCache insert (no eviction)", |b| {
        let mut cache = LruCache::with_size(100_000);
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("TtlCache insert (no eviction)", |b| {
        let mut cache = TtlCache::builder().ttl(Duration::from_secs(3600)).build();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("LruTtlCache insert (no eviction)", |b| {
        let mut cache = LruTtlCache::builder()
            .size(100_000)
            .ttl(Duration::from_secs(3600))
            .build();
        let mut key = 0;
        b.iter(|| {
            cache.cache_set(key, key * 2);
            key += 1;
        })
    });

    group.bench_function("ExpiringCache insert", |b| {
        let mut cache: ExpiringCache<usize, ExpiringValue> = ExpiringCache::new();
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
    let mut lru = LruCache::with_size(capacity);
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
        .size(capacity)
        .ttl(Duration::from_secs(3600))
        .build();
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
        let mut cache = UnboundCache::new();
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

criterion_group!(
    benches,
    bench_cache_hits,
    bench_cache_misses_and_inserts,
    bench_eviction_overhead,
    bench_lock_synchronization
);
criterion_main!(benches);
