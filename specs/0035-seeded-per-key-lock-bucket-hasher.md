# 0035 - seeded per-key lock-bucket hasher

Status: Implemented

## Current state

`sync_writes = "by_key"` on `#[cached]` serializes concurrent calls for the same cache key
through a bucketed per-key lock. Each `by_key` static is wrapped in a `KeyedCache<C, B>` that
holds a fixed-size array of bucket locks and a `RandomState` hasher (`src/lib.rs:773-807`):

```rust
pub struct KeyedCache<C, B> {
    cache: C,
    buckets: Box<[Arc<B>]>,
    hasher: std::collections::hash_map::RandomState,
}
```

`bucket_for(key)` hashes the key with the per-static `RandomState` and selects a bucket by
modulo. The hasher is initialized once at static construction time via `RandomState::new()`,
which seeds from a process-random source.

## Design decisions recorded here

**The hasher is seeded per-static, not globally.** Each `by_key` static gets an independent
`RandomState`. This means two different `#[cached]` functions with `by_key` hash the same key
to potentially different buckets, which is fine: bucket assignment only needs to be consistent
within one static over the lifetime of a process.

**A randomly seeded hasher prevents hash-flooding attacks.** If the bucket assignment were
determined by a fixed seed, an attacker who knows the key space could craft inputs that all hash
to the same bucket, collapsing N-bucket parallelism to serial execution. The process-random seed
makes the bucket assignment unpredictable across process restarts and between processes.

**`KeyedCache` is `#[doc(hidden)]` and not a stable public API.** The only stable surface is
that it `Deref`s to the inner cache lock `C`, so a named `by_key` static (`FN_CACHE.read()`,
`FN_CACHE.write()`, `.lock()`) works the same as any other generated static. The bucket vector
and hasher are private fields; callers cannot observe or influence bucket assignment.

**`bucket_for` uses `BuildHasher::hash_one`.** This avoids constructing a `Hasher` manually and
is the idiomatic way to hash a single value with a `BuildHasher` (`src/lib.rs:803-806`).

## Notes

- `src/lib.rs:773-807` contains the full `KeyedCache` implementation.
- The CHANGELOG `[3.0.0-rc.4]` section notes: "`sync_writes = "by_key"` bucket selection seeds
  from a per-static `RandomState` instead of a fixed seed."
- The number of buckets is controlled by `sync_writes_buckets` on `#[cached]` (default: the
  number of logical CPUs, clamped). More buckets reduce contention at the cost of memory.
