# Spec

Feature inventory for the `cached` workspace: every part of the public surface, documented with
its implementation status. Each row links a per-feature doc describing the normative behavior;
the docs cross-reference the [design records](design/README.md) for the reasoning behind each
decision.

## Feature status

Status values: `done` (implemented and covered by tests), `pending` (documented, not yet built;
the default), `research` (needs investigation or design before it can be built). Keep each row's
status current with `spec.py set`. The whole shipped surface is `done`; open design directions
live in the [design records](design/README.md), not as rows here.

| Feature | Status | Spec |
|---------|--------|------|
| Unbound cache | done | [store-unbound.md](store-unbound.md) |
| LRU cache | done | [store-lru.md](store-lru.md) |
| TTL caches | done | [store-ttl.md](store-ttl.md) |
| Per-value expiring caches | done | [store-expiring.md](store-expiring.md) |
| Sharded concurrent caches | done | [store-sharded.md](store-sharded.md) |
| Redis backend | done | [store-redis.md](store-redis.md) |
| Disk (redb) backend | done | [store-redb.md](store-redb.md) |
| `#[cached]` macro | done | [macro-cached.md](macro-cached.md) |
| `#[once]` macro | done | [macro-once.md](macro-once.md) |
| `#[concurrent_cached]` macro | done | [macro-concurrent-cached.md](macro-concurrent-cached.md) |
| Core cache traits | done | [traits-core.md](traits-core.md) |
| Async get-or-set | done | [trait-get-or-set-async.md](trait-get-or-set-async.md) |
| Concurrent cache traits | done | [traits-concurrent.md](traits-concurrent.md) |
| Store builders and eviction callbacks | done | [builders.md](builders.md) |
| Cache metrics | done | [metrics.md](metrics.md) |
| Cargo feature flags | done | [cargo-features.md](cargo-features.md) |

## Conventions

- Each normative statement carries a stable ID (e.g. `UNBOUND-1`, `REDIS-3`). IDs are
  append-only: retire an ID by marking it removed, never reuse the number.
- Specs are document-first: a feature is documented (status `pending`, or `research` if it needs
  design work) before implementation begins. Flip to `done` only once implemented and verified.
- Feature docs are named `<slug>.md` and linked from the table above.
- A feature doc states the shipped behavior and links the relevant [design records](design/README.md)
  (`design/00NN-*.md`) for the decision history. When behavior changes, update the feature doc
  and add or amend a design record.

## Design records

`design/` holds the per-decision log behind this inventory: 3.0 breaking-change items, declined
proposals, and open research directions. See [design/README.md](design/README.md) for the index.
