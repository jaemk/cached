use cached::Cached;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;

// ---------------------------------------------------------------------------
// UnboundCache benchmarks
// ---------------------------------------------------------------------------

fn unbound_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("unbound/insert");
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut cache = cached::UnboundCache::with_capacity(size);
                for i in 0..size {
                    cache.cache_set(i, black_box(i));
                }
            });
        });
    }
    group.finish();
}

fn unbound_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("unbound/get_hit");
    for &size in &[100, 1000] {
        let mut cache = cached::UnboundCache::with_capacity(size);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in 0..size {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

fn unbound_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("unbound/get_miss");
    for &size in &[100, 1000] {
        let mut cache = cached::UnboundCache::with_capacity(size);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in size..size * 2 {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

fn unbound_remove(c: &mut Criterion) {
    let mut group = c.benchmark_group("unbound/remove");
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let mut cache = cached::UnboundCache::with_capacity(size);
                    for i in 0..size {
                        cache.cache_set(i, i);
                    }
                    cache
                },
                |mut cache| {
                    for i in 0..size {
                        black_box(cache.cache_remove(&i));
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// LruCache benchmarks
// ---------------------------------------------------------------------------

fn lru_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru/insert");
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut cache = cached::LruCache::with_size(size);
                for i in 0..size {
                    cache.cache_set(i, black_box(i));
                }
            });
        });
    }
    group.finish();
}

fn lru_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru/get_hit");
    for &size in &[100, 1000] {
        let mut cache = cached::LruCache::with_size(size);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in 0..size {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

fn lru_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru/get_miss");
    for &size in &[100, 1000] {
        let mut cache = cached::LruCache::with_size(size);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in size..size * 2 {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

fn lru_eviction(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru/eviction");
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut cache = cached::LruCache::with_size(size);
                // Insert 2x capacity to trigger evictions
                for i in 0..size * 2 {
                    cache.cache_set(i, black_box(i));
                }
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// TtlCache benchmarks
// ---------------------------------------------------------------------------

fn ttl_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("ttl/insert");
    let ttl = Duration::from_secs(60);
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut cache = cached::TtlCache::with_ttl_and_capacity(ttl, size);
                for i in 0..size {
                    cache.cache_set(i, black_box(i));
                }
            });
        });
    }
    group.finish();
}

fn ttl_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("ttl/get_hit");
    let ttl = Duration::from_secs(60);
    for &size in &[100, 1000] {
        let mut cache = cached::TtlCache::with_ttl_and_capacity(ttl, size);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in 0..size {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

fn ttl_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("ttl/get_miss");
    let ttl = Duration::from_secs(60);
    for &size in &[100, 1000] {
        let mut cache = cached::TtlCache::with_ttl_and_capacity(ttl, size);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in size..size * 2 {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// LruTtlCache benchmarks
// ---------------------------------------------------------------------------

fn lru_ttl_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru_ttl/insert");
    let ttl = Duration::from_secs(60);
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut cache = cached::LruTtlCache::with_size_and_ttl(size, ttl);
                for i in 0..size {
                    cache.cache_set(i, black_box(i));
                }
            });
        });
    }
    group.finish();
}

fn lru_ttl_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru_ttl/get_hit");
    let ttl = Duration::from_secs(60);
    for &size in &[100, 1000] {
        let mut cache = cached::LruTtlCache::with_size_and_ttl(size, ttl);
        for i in 0..size {
            cache.cache_set(i, i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                for i in 0..size {
                    black_box(cache.cache_get(&i));
                }
            });
        });
    }
    group.finish();
}

fn lru_ttl_eviction(c: &mut Criterion) {
    let mut group = c.benchmark_group("lru_ttl/eviction");
    let ttl = Duration::from_secs(60);
    for &size in &[100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let mut cache = cached::LruTtlCache::with_size_and_ttl(size, ttl);
                for i in 0..size * 2 {
                    cache.cache_set(i, black_box(i));
                }
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

criterion_group!(
    unbound_benches,
    unbound_insert,
    unbound_get_hit,
    unbound_get_miss,
    unbound_remove,
);

criterion_group!(
    lru_benches,
    lru_insert,
    lru_get_hit,
    lru_get_miss,
    lru_eviction,
);

criterion_group!(ttl_benches, ttl_insert, ttl_get_hit, ttl_get_miss,);

criterion_group!(
    lru_ttl_benches,
    lru_ttl_insert,
    lru_ttl_get_hit,
    lru_ttl_eviction,
);

criterion_main!(unbound_benches, lru_benches, ttl_benches, lru_ttl_benches);
