# 0007 - ShardedUnboundCache evictions counter

Status: Not implemented (declined)

## Current state

- `ShardedUnboundCache::metrics()` always returns `evictions: None`
  (`src/stores/sharded/unbound.rs:242`) even though its `on_evict` callback fires on
  `cache_remove`.
- Every other sharded store tracks an `AtomicU64` evictions counter.

## Desired work

- Add an evictions counter to the unbound inner so `metrics().evictions` is `Some(n)`.

## Notes

- Declined. An unbound cache has no eviction policy; explicit removes are not evictions in this
  model.
- Leave `evictions: None`. The asymmetry is documented.
