# 0055 - Custom routers keep their inherent methods: `ShardHasher<Q>` over `BorrowedKeyRouting`

Status: Implemented

Supersedes the route choice recorded in
[0052](0052-sharded-borrowed-key-lookups.md)'s "Verdict" and "Outcome" sections. 0052 stays as
history and is not edited; this record replaces its decision, not its inventory.

## Current state

0052 made six inherent lookups on each of the six sharded stores generic over `Borrow<Q>`, and
bounded every one of them on `H: BorrowedKeyRouting`:

```rust
pub trait BorrowedKeyRouting: std::hash::BuildHasher {}
impl<T: std::hash::BuildHasher> BorrowedKeyRouting for T {}
```

(`src/stores/sharded/mod.rs`, near the `ShardHasher` definition.) The marker is a diagnostic alias
for `BuildHasher`, carrying a `#[diagnostic::on_unimplemented]` message. Each store has a private
`shard_of_borrowed<Q>` bounded the same way, which bypasses the `ShardHasher` trait and calls the
free function `routing_hash` directly.

Scale of the surface this created: 140 references to `BorrowedKeyRouting` across 23 files
(`src/lib.rs`, `src/stores/mod.rs`, all six sharded store modules, `AGENTS.md`, `CHANGELOG.md`,
`README.md`, two `specs/` topic docs, and seven test files including two trybuild goldens);
87 references to `shard_of_borrowed` across the seven sharded modules.

0052 recorded its own release blocker for 3.2: three classes of breaking change against the
published 3.1.1.

1. A sharded store built over a hand-written, non-`BuildHasher` `ShardHasher` loses its inherent
   `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek` entirely, owned-key calls included,
   because there is one method per name and its bound covers every caller including `Q = K`.
   Owned access survives only through `ConcurrentCachedExt`/`ConcurrentCachePeek`, both needing
   `.unwrap()`.
2. Argument-inference breakage: `cache.get(&k)` with `k: &K` (or `&Box<K>`/`&Arc<K>`) infers
   `Q = &K` and fails on a missing `Borrow` impl.
3. Generic-helper breakage: a downstream helper bounded only on `H: ShardHasher<K>` stops
   compiling at its own definition and must add `H: BorrowedKeyRouting`.

## Why the 0052 verdict is being reversed

**0052 overstated the cost of the alternative.** It rejected relaxing `ShardHasher<K>` to
`K: ?Sized` and bounding the methods on `H: ShardHasher<Q>` as trading "a compile-time-enforced
restriction for a runtime-silent one", calling it "a correctness landmine for everyone else". That
is not what the shape does. Under it, every `BuildHasher` still reaches every `Q` through the
blanket impl, where owned-versus-borrowed agreement is guaranteed by the `Borrow` contract, exactly
as today. The unenforceable cross-impl consistency requirement arms only for a type that
hand-writes two or more `ShardHasher` impls, which is code that deliberately opted in. It is the
same species of contract as `Borrow`'s own requirement that equal keys hash equally, which the
compiler also cannot check and which every borrowed lookup in this crate already rests on.

**The `hash_one` defect is on a different axis and does not argue for either route.** 0052's
Outcome section records that shard routing through `BuildHasher::hash_one` could send an owned
insert and an equivalent borrowed lookup to different shards, because `ahash::RandomState`
overrides `hash_one` under its `specialize` cfg and dispatches on the static type of the argument.
0044's original blanket impl was `fn shard_hash(&self, key: &K) -> u64 { self.hash_one(key) }`,
which is the shape route 2 would have inherited too. The fix (build a `Hasher` explicitly, feed the
key through `Hash::hash`, `finish`) is independent of which trait bound the methods carry, and it
stays in place under this record. Reverting the route does not reintroduce the defect, and keeping
route 1 would not have prevented it.

**The compatibility ledger favors route 2.** Break classes 1 and 3 disappear; class 2 does not,
because it follows from making the methods generic over `Q` at all, which both routes do.

| Break class | Route 1 (0052, shipped) | Route 2 (this record) |
|---|---|---|
| 1. Hand-written router loses inherent methods, owned calls included | yes | gone |
| 2. `cache.get(&k)` with `k: &K` fails inference | yes | unchanged, still present |
| 3. Downstream helper on `H: ShardHasher<K>` stops compiling | yes | gone for owned calls; only new borrowed calls add a bound |

It also removes `BorrowedKeyRouting` from the public surface along with its `on_unimplemented`
machinery, its two trybuild goldens, and its prelude entry.

**There is no upstream contract to appeal to for the route-1 framing.** `BuildHasher::hash_one`
in `core` (`library/core/src/hash/mod.rs`, the `fn hash_one` docs on 1.96) has no note to
implementers and states no requirement that an override agree with `build_hasher` plus
`Hash::hash`. Route 1's premise, that restricting to `BuildHasher` buys a checkable guarantee, is
weaker than it reads: what it actually buys is the `Borrow` contract, which route 2 gets for the
blanket impl just as well.

## Desired work

### 1. Relax the trait and its blanket impl

```rust
pub trait ShardHasher<K: ?Sized>: Clone + Send + Sync + 'static {
    fn shard_hash(&self, key: &K) -> u64;
}

impl<K, S> ShardHasher<K> for S
where
    K: std::hash::Hash + ?Sized,
    S: std::hash::BuildHasher + Clone + Send + Sync + 'static,
{
    fn shard_hash(&self, key: &K) -> u64 {
        routing_hash(self, key)
    }
}
```

`routing_hash` gains `K: Hash + ?Sized` and keeps its explicit `build_hasher` / `Hash::hash` /
`finish` body unchanged. Coherence is unaffected: an existing `impl ShardHasher<u64> for FibHasher`
still compiles, and a type implementing `BuildHasher` still cannot carry a hand-written impl.

### 2. Rebound the 36 inherent methods

Per method, `H: BorrowedKeyRouting` becomes `H: ShardHasher<Q>`. Nothing else in the signatures
changes.

```rust
pub fn get<Q>(&self, k: &Q) -> Option<V>
where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
    H: ShardHasher<Q>,
```

### 3. Route `shard_of_borrowed` through the trait

`shard_of_borrowed<Q>` is rebounded on `H: ShardHasher<Q>` and its body becomes
`self.inner.hasher.shard_hash(k)` instead of `routing_hash(&self.inner.hasher, k)`. This is a
simplification route 1 could not have: the owned and borrowed paths become literally the same call,
so the two cannot drift, and a custom router's own `shard_hash` is honored on the borrowed path
rather than being bypassed. Every doc comment on these helpers that explains the `routing_hash`
bypass needs rewriting to match.

### 4. Delete `BorrowedKeyRouting`

Remove the trait, its blanket impl, its `on_unimplemented` attribute, its crate-root export
(`src/lib.rs`), and its prelude entry. Move the useful half of the diagnostic to
`#[diagnostic::on_unimplemented]` on `ShardHasher` itself, aimed at the case it can actually catch:
a hand-written router that has not implemented `ShardHasher<{K}>`.

### 5. Document the consistency contract on `ShardHasher`

Phrase it the way `Borrow` phrases its own, and say plainly that the compiler cannot check it:

> If a type implements `ShardHasher` for more than one key type, all of those impls must agree on
> keys that compare equal. Concretely, for `K: Borrow<Q>`, `shard_hash(&k)` must equal
> `shard_hash(k.borrow())`. Violating this routes an owned insert and an equivalent borrowed lookup
> to different shards, producing a miss on an entry that is present. Types reaching `ShardHasher`
> through the blanket `BuildHasher` impl satisfy this automatically.

Include a copyable guard test in the rustdoc so a custom-router author can pin the property for
their own type.

### 6. Update the surrounding material

`CHANGELOG.md` (rewrite the 3.2 Breaking Changes entries: classes 1 and 3 are gone, class 2 stays,
plus the `ShardHasher<K: ?Sized>` signature relaxation and the removal of `BorrowedKeyRouting`
from the prelude and crate root), `specs/store-sharded.md` (SHARD-15),
`specs/traits-concurrent.md` (CTRAIT-2), `AGENTS.md` (the marker-subtrait line),
`src/lib.rs` crate docs including the family comparison and the generic-helper migration snippet,
and `README.md` by regeneration only (`cargo readme`, per 0049; never hand-edited).

## Pitfalls

- **The missing-impl diagnostic is worse than route 1's, and `on_unimplemented` cannot fix it.**
  When a hand-written router implements exactly one `ShardHasher<K>`, rustc resolves `Q` to that
  one impl and reports a type mismatch rather than an unsatisfied bound, so no
  `on_unimplemented` message fires. Measured on 1.96 against a router implementing only
  `ShardHasher<UserId>`, calling `store.get(&7u64)`:

  ```
  error[E0308]: mismatched types
      |
      |     assert_eq!(t.get(&7u64), Some(&9));
      |                  --- ^^^^^ expected `&UserId`, found `&u64`
  ```

  This is legible but does not tell the author to add `impl ShardHasher<u64>`. Mitigate in the
  `ShardHasher` rustdoc, and pin the message with a trybuild case so it cannot silently degrade.
  Do not spend effort trying to make `on_unimplemented` fire here; inference collapses before any
  bound goes unsatisfied.

- **Break class 2 is not fixed and must not be claimed as fixed.** Measured under the route-2
  shape, `s.get(&kref)` with `kref: &String` still fails with
  `the trait bound `String: Borrow<&String>` is not satisfied`, identically to today. The existing
  `&&K` / `&Box<K>` / `&Arc<K>` documentation stays exactly as written.

- **Do not reintroduce `hash_one` anywhere on the routing path.** `routing_hash` keeps its explicit
  construction, and `DefaultShardHasher` keeps having no `hash_one` override. The guard tests from
  the previous cycle (`default_shard_hasher_hash_one_agrees_for_newtype_and_primitive` in
  `src/stores/sharded/unbound.rs`, and the `TypeDispatchingHasher` case in
  `tests/sharded_newtype_key_routing_parity.rs`) must keep passing unchanged.

- **The per-shard maps are not part of this change**, and they are safe by two different
  mechanisms depending on the store family. `ShardedUnboundCache`, `ShardedTtlCache`, and
  `ShardedExpiringCache` back their shards with `HashMap<K, V, DefaultShardHasher>` regardless of
  `H`. The three LRU-family stores (`ShardedLruCache`, `ShardedLruTtlCache`,
  `ShardedExpiringLruCache`) instead hold `LruCache<K, V>` on `DefaultHashBuilder =
  ahash::RandomState` (`src/stores/sharded/lru.rs:22`, `lru_ttl.rs:30`, `expiring_lru.rs:27`,
  `src/stores/mod.rs:41`), which is safe by a second mechanism: `LruCache::hash`
  (`src/stores/lru.rs:478-486`) hand-builds a `Hasher` and feeds the key through `Hash::hash` /
  `finish` instead of calling `hash_one`. Neither mechanism changes with `H`.

- **`?Sized` on the trait parameter removes an implied bound.** `H: ShardHasher<K>` no longer
  implies `K: Sized` at use sites. This is a relaxation and existing impls compile unchanged, but
  it can shift inference in rare downstream code, so it belongs in the CHANGELOG.

- **Owned-key `set` and `get_or_set_with` stay owned**, as in 0052. Nothing here touches them.

## Verification

Feasibility was checked ahead of this record with a standalone probe under `./local/` (gitignored,
not durable). Every check it made must be promoted to a committed test:

- A hand-written, non-`BuildHasher` router keeps its inherent owned `get`/`remove`/`remove_entry`/
  `delete`/`contains`/`peek`. This is break class 1 and is the whole point of the change.
- The same router, given a second `ShardHasher<Q>` impl, gets working borrowed lookups: an owned
  insert is found through the borrowed form.
- A downstream generic helper bounded only on `H: ShardHasher<K>` compiles while doing owned
  lookups (break class 3). `tests/sharded_generic_helper_bounds.rs` is the existing home for this
  and should be rewritten rather than replaced.
- Borrowed lookups through the blanket impl keep working on all six stores, including inference
  from `&str` to `Q = str`.
- A trybuild case pinning the E0308 message for a router missing the second impl. The two existing
  goldens (`tests/ui/sharded_owned_key_requires_build_hasher.rs` and
  `sharded_borrowed_key_requires_build_hasher.rs`) test a restriction that no longer exists and
  should be replaced by this one, not regenerated.
- The existing routing-parity suite passes unchanged on both stable and nightly.

## Notes

This record does not touch the single-owner half of the `hash_one` divergence. `DefaultHashBuilder`
is still `ahash::RandomState` (`src/stores/mod.rs`), so a single-owner store keyed on a newtype over
a primitive still misses on a borrowed lookup under nightly. The fix there is to make
`DefaultHashBuilder` the same kind of non-overriding newtype `DefaultShardHasher` already is, which
changes a public type default and wants its own record.

## Outcome

Implemented as recommended. `ShardHasher<K: ?Sized>` carries the blanket impl over `K: Hash +
?Sized`; `routing_hash` already had `K: Hash + ?Sized` from a prior fix, so it needed no edit here
(`src/stores/sharded/mod.rs`). All 36 inherent lookups and the six `shard_of_borrowed` helpers are
rebounded on `H: ShardHasher<Q>`, and each `shard_of_borrowed` now calls
`self.inner.hasher.shard_hash(k)` directly instead of `routing_hash(&self.inner.hasher, k)`, so the
owned and borrowed paths are the same call and a custom router's own `shard_hash` is honored on the
borrowed path rather than bypassed. `BorrowedKeyRouting` is deleted from the trait definitions, the
crate root, and the prelude, along with the rustdoc that existed only to explain it.
`ShardHasher` picked up the `#[diagnostic::on_unimplemented]` with four notes described under
"Delete `BorrowedKeyRouting`" (`src/stores/sharded/mod.rs`). `CHANGELOG.md`, `specs/store-sharded.md`
(SHARD-15, plus a stale SHARD-3 reference), `specs/traits-concurrent.md` (CTRAIT-2), and `AGENTS.md`
are resynced; `README.md` is regenerated via `cargo readme`, not hand-edited.

New committed tests: `tests/sharded_custom_router_lookups.rs` (9 tests), a rewritten
`tests/sharded_generic_helper_bounds.rs`, `mod custom_shard_hasher_doc_contract` in `src/lib.rs`,
two new in-module tests plus two new doctests in `src/stores/sharded/mod.rs`. The two
`*_requires_build_hasher` trybuild goldens (`tests/ui/sharded_owned_key_requires_build_hasher.rs`,
`sharded_borrowed_key_requires_build_hasher.rs`), which tested a restriction this record removes,
are deleted and replaced by `tests/ui/sharded_router_missing_borrowed_impl.rs` (E0308) and
`tests/ui/sharded_helper_missing_borrowed_bound.rs` (E0277, where the new `on_unimplemented` text
fires). `tests/ui/sharded_non_clone_shard_hasher.stderr` picked up a one-line refresh
(`ShardHasher<K>` -> `ShardHasher<K: ?Sized>`) as a byproduct of the trait relaxation.

`make ci` is green. `make check` exits 0: fmt clean, `cargo readme` output byte-identical to
`README.md`, `cargo clippy --all-features --all-targets --examples --tests -- -D warnings` clean,
`cargo fmt --check` clean, help coverage clean. `make tests/all-features` with the Makefile's
docker redis container up: 2787 tests passed, 0 failed, across 93 binaries (lib, integration,
trybuild, doc tests), including the new and rewritten suites: `sharded_custom_router_lookups`,
`sharded_generic_helper_bounds`, `v3_macros_ui`, `sharded_router_agreement_per_store` (21 tests,
agreeing and disagreeing routers on all six stores), `sharded_router_trait_and_async_surface` (16
tests, `ConcurrentCached`/`ConcurrentCachedExt`/`ConcurrentCachePeek` on all six stores plus async
on four), and `sharded_unsized_key_lookups` (7 tests, `Path`/`OsStr`/`[u32]`/`Arc<str>`). Feature
slices `tests/no-default`, `tests/default`, `tests/proc-macro`, `tests/time-stores`, `tests/ahash`,
`tests/async`, `tests/disk-store`: 0 failures. `make examples`: all 10 examples run. On nightly
(`cargo +nightly test --all-features`), over the five routing files: 61 passed, 0 failed, including
`sharded_newtype_key_routing_parity` (8), `sharded_custom_router_lookups` (9), and
`sharded_generic_helper_bounds` (3). The pinned
`type_dispatching_hasher_catches_a_hash_one_regression_on_any_toolchain` guard passes, so the
previous cycle's `hash_one` fix is intact under this change.

One flake observed during verification, recorded rather than omitted: `tests/disk-store-sync`
failed once on `tests/v3_redb_races` (5 passed, 1 failed), then passed 6/6 on an isolated rerun.
This is a pre-existing timing-dependent flake, not attributable to this change: `git status
--porcelain` shows this change touches no redb or disk-store file.

Four claims in this record's own Pitfalls and Verification sections did not hold up as written and
are corrected here rather than silently fixed:

- **The inference-collapse pitfall understated its own scope.** It documented the E0308 collapse
  (rustc resolving `Q` to the router's one impl and reporting a type mismatch instead of an
  unsatisfied bound) only for a concrete hand-written router with a single impl. The same collapse
  happens for a *generic* type parameter too: a helper bounded on `H: ShardHasher<String>` calling
  `cache.get("a")` resolves `Q = String` from the only in-scope bound before any bound is checked,
  and reports `expected &String, found &str`, not a missing-bound error. Verified directly against
  the shipped trait shape: `H: ShardHasher<String> + ShardHasher<str>` with `store.get("a")`
  compiles with no turbofish; `H: ShardHasher<String>` alone gives the same E0308 shape. So the
  collapse only pre-empts the bound check when the needed bound is absent, and the fix in real code
  is always to add the missing bound, never to turbofish. Turbofish (`get::<str>("a")`) appears once
  in the tree, in `tests/ui/sharded_helper_missing_borrowed_bound.rs`, purely as a fixture device to
  force the unsatisfied-bound error into view so the diagnostic text can be pinned; it is not
  guidance for callers.
- **The `on_unimplemented` attribute was worth keeping, contrary to this record's doubt, and the
  "single-impl" framing in the Pitfalls section was also too broad.** The Pitfalls section said the
  diagnostic could not fire for the case it cared about and implied limited value in adding it at
  all; it does fire, with the full four-note message, in two of three concrete diagnostic shapes.
  The E0308 collapse it documented holds only for a concrete router with exactly one `ShardHasher`
  impl, where `Q` has nothing else to resolve to. A concrete router with two or more impls has
  nothing to collapse onto either: the bound is genuinely unsatisfied and the full E0277 message
  fires the same as it does for a generic `H`. Three diagnostic shapes are now pinned: E0308 for a
  single-impl concrete router (`tests/ui/sharded_router_missing_borrowed_impl.stderr`), E0277 for a
  multi-impl concrete router missing the third impl
  (`tests/ui/sharded_router_multi_impl_missing_borrowed_impl.stderr`), and E0277 for a generic `H`
  missing the bound (`tests/ui/sharded_helper_missing_borrowed_bound.stderr`).
- **The "Rebound the 36 inherent methods" sketch omitted a bound a borrowed-form helper actually
  needs.** `H: ShardHasher<Q>` alone is not a compiling signature for a helper that also names the
  store type, because the store's inherent `impl` block is itself bounded on `H: ShardHasher<K>`.
  The compiling shape is the conjunction, `H: ShardHasher<K> + ShardHasher<Q>`. `src/lib.rs`'s
  "Custom shard hashers" blockquote and `tests/sharded_generic_helper_bounds.rs`
  (`lookup_borrowed`, `lookup_generic_borrowed`) both carry this now.
- **The consistency contract's failure mode is now pinned, not just documented.** The "Document the
  consistency contract on `ShardHasher`" section asked for the contract to be written down;
  nothing in the desired work pinned what violating it actually costs.
  `disagreeing_router_impls_lose_a_present_entry_through_its_borrowed_form` in
  `tests/sharded_custom_router_lookups.rs` now does: a router whose two `ShardHasher` impls
  deliberately disagree loses a present entry through every borrowed form (owned `get` hits, all
  six borrowed forms miss or no-op, `len()` unchanged throughout). The test's own doc comment notes
  this pins a footgun and does not endorse the pattern.

Break class 2 is confirmed present exactly as predicted, with no change in shape:
`s.get(&kref)` with `kref: &String` still fails with
`` the trait bound `String: Borrow<&String>` is not satisfied ``. Nothing in the tree claims
otherwise; `CHANGELOG.md` and `tests/sharded_generic_helper_bounds.rs::lookup_all` both carry it.
This diagnostic is quoted as migration text in four places (`CHANGELOG.md`, `src/lib.rs`,
`README.md`, `specs/store-sharded.md`) but was asserted nowhere until now:
`tests/ui/sharded_double_reference_key_inference.rs` and its `.stderr` pin the failing form
verbatim, so the quoted migration text cannot drift from the compiler's actual output.

Known limitations, updated by a later coverage pass:

- **Resolved.** The disagreeing-router test originally ran only on `ShardedUnboundCache`, leaving
  the other five stores' shared `shard_of_borrowed` path as an assumption rather than a proof.
  `tests/sharded_router_agreement_per_store.rs` (21 tests) now runs both an agreeing and a
  disagreeing router on all six sharded stores.
- **Resolved.** The original disagreeing-router test recomputed the documented
  `(hash >> 32) & (shards - 1)` shard-index formula itself, because there was no public hasher
  accessor to read it from, so its explanatory assertion would have drifted silently if that
  formula ever changed. `tests/sharded_router_agreement_per_store.rs` reads `shard_sizes()`, a
  public accessor, instead; no formula is recomputed anywhere in that file.
- **Partially resolved.** `tests/sharded_router_trait_and_async_surface.rs` (16 tests) now exercises
  a hand-written router through `ConcurrentCached`/`ConcurrentCachedExt`/`ConcurrentCachePeek` on
  all six sharded stores, and through the async trait forms
  (`ConcurrentCachedAsync`/`ConcurrentCachedAsyncExt`/`ConcurrentCachePeekAsync`) on four of six
  (`ShardedUnboundCache`, `ShardedLruCache`, `ShardedExpiringLruCache`, `ShardedLruTtlCache`).
  `ShardedExpiringCache` and `ShardedTtlCache` are not exercised on the async surface.
- **Not addressed.** No mutation testing was performed; the shared working tree across concurrent
  shards made deliberately introducing bugs unsafe. The structural substitute is that the agreeing
  and disagreeing router suites make contradictory assertions over the same code path
  (`shard_of_borrowed`), so a routing regression that satisfied one suite would very likely fail
  the other, without a mutation run proving it.

The single-owner `DefaultHashBuilder` half of the `hash_one` divergence, scoped out in "Notes"
above, stays out of scope and unfixed.
