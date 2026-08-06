# 0018 - Escape redis key segments

Status: Implemented

## Current state

The key layout is `{namespace}:{prefix}:{key}` with fixed arity and percent-escaped fields:

- Every field contributes a field and a separator, so a key always carries exactly two separators.
  An empty namespace is an empty leading field (`:{prefix}:{key}`), not a missing one.
- `escape_key_field` percent-escapes `:` to `%3A` and `%` to `%25`. A field containing neither is
  left byte-identical, so ordinary keys are unchanged and stay readable in redis-cli.
- The namespace is canonicalized first by trimming trailing colons, so `"ns"` and `"ns:"` are the
  same namespace. That is what lets the default namespace be written as `cached-redis-store:`.

Before this change the fields were joined unescaped and an empty namespace dropped its field, so
two caches could share one keyspace: namespace="a:b" collided with namespace="a", prefix="b" (both
wrote `a:b:p:k`), and a cache with an empty namespace and prefix `p` had clear scope `p:*`, which
covered every key of a cache whose namespace was `p`, whatever that cache's own prefix was. Both
were documented, and a unit test asserted the first collision.

## Design decisions recorded here

**Percent-escaping, not backslash-escaping.** `%` is not a glob metacharacter, so an escaped field
can be embedded in the `SCAN MATCH` pattern without the two escape schemes interacting.

**Fixed arity, not a variable one.** Dropping the field of an empty namespace saved one character
and cost two properties: `("", "p", "k")` and `("p", "", "k")` had to be told apart by separator
count, and the *scope* of a namespace-less cache became a prefix of every key under a namespace
equal to its prefix. Always writing both separators makes the two keyspaces structurally disjoint,
since no non-empty namespace can produce a leading `:`.

**Length-prefixing was rejected.** It is the other way to make the join unambiguous, but it puts a
byte count in front of every key, so a user cannot hand-write a `SCAN MATCH` pattern without
computing lengths, and it charges every key for a case that only arises when a field contains a
separator.

**Distinct triples map to distinct keys.** Escaping removes `:` from the namespace and prefix
fields and the arity is fixed, so the first two `:` in a key are always its separators: splitting
there recovers the three escaped fields verbatim, and percent-unescaping recovers the fields
themselves. The whole triple is reconstructible from the key, so the mapping is injective.
`every_field_is_recoverable_from_the_key` in `src/stores/redis.rs` implements that decoder and
checks it round-trips.

Injectivity is over the *canonical* namespace: `"ns"`, `"ns:"` and `"ns::"` are identified by the
trailing-colon trim, which predates this change and is what the documented default-namespace
spelling relies on. That identification is deliberate normalization, not a collision.

**`clear_match_pattern` shares the framing.** The scan glob is built from the same escaped fields
through the same `join_key_fields` helper, then glob-escaped (`*`, `?`, `[`, `]`, `\`), so its
literal part is byte-for-byte the scope of the keys the cache writes. The two escapes touch
disjoint characters: percent-escaping emits only `%` and hex digits, glob-escaping inserts only
`\`. Both cross-cache overlaps are closed: namespace="a:b", prefix="p" scans `a%3Ab:p:*` and no
longer covers namespace="a", prefix="b:p", and an empty namespace scans `:p:*` and no longer
covers namespace="p".

## Notes

- Wire-format (key layout) change. A cache with an empty namespace, or whose namespace, prefix or
  keys contain `:` or `%`, writes at a different key than in 2.x. The value envelope's `version`
  field lives inside the value, so it cannot describe the key layout: old entries are not found,
  are recomputed and rewritten at the new key, and are left in place to expire with their TTL (or
  to be removed by hand when the cache has no TTL). Stated in the `RedisCache` "Format stability"
  docs, in [../store-redis.md](../store-redis.md) (REDIS-6), and in the migration guide.
- The empty-prefix rejection in `build` stays, but its rationale narrowed with the fixed arity: an
  empty prefix is now scoped (`{namespace}::*`) rather than matching the whole namespace. It is
  still rejected because the prefix is what distinguishes logical caches sharing a namespace.
- Tests: `src/stores/redis.rs` unit tests (escaped layout, fixed arity, injectivity, key/pattern
  agreement, glob interaction, empty-namespace scope isolation), `tests/frozen_format_golden.rs`
  (`redis_key_layout`, live server) and `tests/v3_redis_key_escaping_coverage.rs` (real glob
  semantics, `AsyncRedisCache`, UTF-8 fields, scope isolation).
