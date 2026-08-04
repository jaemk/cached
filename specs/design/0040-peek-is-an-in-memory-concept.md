# 0040 - Peek is an in-memory concept: `ConcurrentCachePeekAsync`, no peek on the IO stores

Status: Implemented

## Previous state

`ConcurrentCachePeek<K, V>: ConcurrentCacheBase` provided a side-effect-free
`cache_peek(&self, &K) -> Result<Option<V>, Self::Error>` (plus a defaulted `peek` alias),
implemented by the six sharded stores. There was no async counterpart: calling `async_cache_peek`
on a sharded store was an E0599 whose rustc suggestion was actively wrong, proposing that the
caller append `.await` to the non-future sync `cache_peek`.

`RedisCache`, `RedbCache`, and `AsyncRedisCache` implemented no peek trait, which their rustdoc
already stated, but nothing recorded the reasoning, so the omission read as an oversight to be
filled in rather than a decision.

## The rule

Peek is an in-memory concept. One principle settles both halves of this record.

The peek contract is a set of negative guarantees: a peek performs no recency (LRU) promotion,
no TTL refresh, no hit/miss metrics accounting, and no lazy removal of an expired entry; an
expired entry reads as `None`. The value of a peek bound is entirely in what the operation does
NOT do, so the bound is only worth having where those negatives are both meaningful and cheap to
uphold.

### `ConcurrentCachePeekAsync` is a separate trait with a required method

`ConcurrentCachePeekAsync<K, V>: ConcurrentCacheBase` mirrors `ConcurrentCachePeek` on the async
side, with `async_cache_peek(&self, &K) -> Result<Option<V>, Self::Error>` as a REQUIRED method
and deliberately no default body.

The alternative considered was adding a defaulted `async_cache_peek` to `ConcurrentCachedAsync`:
non-breaking, and it avoids a new type. It was rejected because a default body can only be written
in terms of the trait's own methods, i.e. an ordinary `async_cache_get`. That satisfies the
signature while violating every negative guarantee in the list above: on an LRU store it promotes,
on a refresh-on-hit TTL store it extends the entry's life, and on any store it moves the hit/miss
counters. Generic code bounded on the trait could then rely on none of the contract, which leaves
the bound decorative. A separate trait with no default keeps the guarantee load-bearing: an
implementor must write a genuinely side-effect-free read, so satisfying the bound implies the
behavior.

### `RedisCache`, `RedbCache`, and `AsyncRedisCache` implement neither peek trait

This affirms the pre-existing rustdoc rather than reversing it. For an IO-backed store there is no
client-side recency chain to skip and no client-side TTL state to leave alone (the server or the
database owns expiry), and the hit/miss distinction a peek is supposed to stay out of is not
tracked in a way that makes peeking meaningfully different from getting. Every negative guarantee
is therefore either vacuous or unobservable, while the positive cost is unchanged: the operation
is still a full network round-trip or a disk read transaction.

Implementing peek there would advertise a cheapness the store cannot deliver. The concrete harm is
in generic code: a function bounded on `ConcurrentCachePeek` reads as "cheap, side-effect-free,
safe in a hot loop", and adding IO implementors would let such a function silently become
IO-bound (or, on redb, take a read transaction) when instantiated with a redis or disk store.

The consequence is accepted and stated plainly: generic `ConcurrentCachePeek` /
`ConcurrentCachePeekAsync`-bounded code accepts only the six sharded stores, and `AsyncRedisCache`
has no side-effect-free read at all. Callers who want a read against redis or redb use
`cache_get` / `async_cache_get` and accept its effects.

## Implementor set

- `ConcurrentCachePeek`: the six sharded stores (`ShardedUnboundCache`, `ShardedLruCache`,
  `ShardedTtlCache`, `ShardedLruTtlCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`),
  each with `Self::Error = Infallible`.
- `ConcurrentCachePeekAsync`: the same six stores, `Self::Error = Infallible`, each implementation
  delegating to its existing sync `cache_peek`. The sharded stores are self-synchronizing and
  never block on IO, so there is nothing to await; the async method exists so that generic async
  code can name a peek bound, not because the operation itself is asynchronous.
- `RedisCache`, `RedbCache`, `AsyncRedisCache`: neither trait.

`ConcurrentCachePeekAsync` joins `cached::prelude` alongside `ConcurrentCachePeek`.

## Observable surface that changes

- New public trait `ConcurrentCachePeekAsync`, and a new prelude entry. Purely additive: no
  existing signature changes.
- `async_cache_peek` on a sharded store now compiles. It previously produced an E0599 with a
  misleading rustc suggestion to `.await` the sync `cache_peek`, which is not a future; removing
  that dead end is a secondary benefit of adding the trait rather than defaulting a method on
  `ConcurrentCachedAsync`.
- Nothing changes for the IO stores. Their behavior and their docs are unchanged; this record
  makes the existing omission a stated decision.

## Notes

- See [traits-concurrent.md](../traits-concurrent.md) CTRAIT-3 for the shipped trait statement.
- Related: 0023 (why `CachedPeek` and `CachedRead` stay separate on the single-owner side, for the
  same reason: a trait bound that enforces nothing is not worth having).
