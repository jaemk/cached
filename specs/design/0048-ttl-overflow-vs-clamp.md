# 0048 - Extreme TTL: overflow-to-never-expires vs. clamp-to-real-deadline

Status: Not implemented

## Current state

Two families disagree on what an extreme TTL (at or beyond `Duration::MAX`, i.e. beyond what
`Instant::checked_add` can represent) does to an entry's deadline, and two doc comments claim
they agree.

- Single-owner stores keep the configured TTL as a raw `Duration` field (`TtlCache::ttl`,
  `src/stores/ttl.rs:114` sets it via the builder; `CacheTtl::set_ttl` at `src/stores/ttl.rs:766`
  assigns `self.ttl = ttl` unclamped). `compute_expires_at` then does
  `now.checked_add(ttl)`, which overflows to `None` for a TTL near `Duration::MAX`
  (`src/stores/ttl.rs:315-321`, identical shape at `src/stores/lru_ttl.rs:413-419`). `None`
  means "never expires" everywhere in this crate (same encoding as a disabled/zero TTL).
  `TtlCache::ttl()` still reports the raw configured value, so `CacheTtl::ttl(&c)` after
  `set_ttl(Duration::MAX)` returns `Some(Duration::MAX)` (`src/stores/ttl.rs:756-763`; pinned at
  `tests/v3_single_owner_zero_ttl.rs:838-839`) even though the computed deadline is `None`.
- Sharded stores store the TTL as an `AtomicU64` of nanoseconds. `encode_ttl` clamps on the way
  in: `ttl.as_nanos().min(u64::MAX as u128) as u64` (`src/stores/sharded/mod.rs:182-184`). So a
  `Duration::MAX` (or anything else whose nanosecond count doesn't fit `u64`) is silently
  rounded down to `u64::MAX` nanoseconds, about 584.9 years. `compute_expires_at` on each sharded
  store loads that already-clamped value and calls `now.checked_add(ttl)`
  (`src/stores/sharded/ttl.rs:187-195`, same shape at `src/stores/sharded/lru_ttl.rs:211-219`);
  the clamp means this `checked_add` practically never overflows, so a real, ~584-year-out
  `Instant` comes back instead of `None`. `decode_ttl` (`src/stores/sharded/mod.rs:190-195`) only
  special-cases nanos `== 0` (-> `None`, expiry disabled); every other value, including the
  clamped `u64::MAX`, round-trips through `ttl_duration_impl` (`src/stores/sharded/ttl.rs:122-124`,
  `src/stores/sharded/lru_ttl.rs:108-109`) into the `ConcurrentCacheTtl::ttl()` getters
  (`src/stores/sharded/ttl.rs:609-611`, `src/stores/sharded/lru_ttl.rs:746-748`).
- Net observable split: after `set_ttl(Duration::MAX)`, `TtlCache::ttl()` returns
  `Some(Duration::MAX)` but the stored deadline is "never expires"; `ShardedTtlCache::ttl()`
  returns `Some(Duration::from_nanos(u64::MAX))` and the stored deadline is a real `Instant`
  about 584 years out. Both sides' `ttl()` getters report a real (if different) configured
  value; the divergence is in what the *entry's deadline* becomes, visible through
  `cache_peek_expires_at` / `cache_expires_at`.

**Two doc comments are wrong and must be corrected regardless of the decision below.**
`src/stores/ttl.rs:311-313` says overflow "returns `None`: the entry never expires, matching the
sharded TTL stores." `src/stores/lru_ttl.rs:409-411` says the identical thing. Both are false as
of today's code: the sharded stores clamp before they ever reach `checked_add`, so they do NOT
match - they stamp a real, ~584-year deadline instead of `None`. The sharded side's own comments
are accurate and already say so: `src/stores/sharded/ttl.rs:184-186` and
`src/stores/sharded/lru_ttl.rs:208-210` both read "TTL is clamped to u64::MAX nanos (~584
years), so `checked_add` overflow is practically unreachable."

**Existing test pins (corrected from the intake brief - see Notes).** The single-owner
overflow-to-never-expires behavior is pinned by
`cache_set_with_ttl_overflow_stores_never_expiring_entry` (`src/stores/ttl.rs:1183-1198`, mirror
in `src/stores/lru_ttl.rs:1412-1431`) and by `peek_expires_at_overflowing_ttl_reports_no_deadline`
(`src/stores/ttl.rs:3012-3027`, mirrors `peek_expires_at_reports_no_deadline_under_ttl_overflow`
in `src/stores/lru_ttl.rs:3836` and `src/stores/ttl_sorted.rs:6308`). The sharded clamp-to-real-
deadline behavior is ALSO already pinned in-crate, contrary to the brief that reached this task:
`peek_expires_at_extreme_ttl_is_clamped_not_overflowed` exists in both
`src/stores/sharded/ttl.rs:3820-3839` and `src/stores/sharded/lru_ttl.rs:2910-2929`. On top of
those per-family unit tests, the per-key expiry read work (design 0047) added a test that pins
both sides against each other on purpose:
`extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families`
(`tests/v3_per_key_expiry_read.rs:1066-1170`). Its doc comment (`tests/v3_per_key_expiry_read.rs
:1045-1065`) states the split is "a pre-existing, deliberate representation difference ... not a
bug" and exists so a future change that collapses the two representations "fails loudly here."
Any reconciliation work in this record must update this test - see Desired work.

## Desired work

Two separable pieces. Either can ship alone; the doc fix does not depend on the reconciliation
decision.

### 1. Fix the two false doc comments (do this regardless)

Smallest possible change: correct `src/stores/ttl.rs:311-313` and `src/stores/lru_ttl.rs:409-411`
to state the true, current behavior - overflow yields "never expires" here, but the sharded
stores clamp the TTL before computing the deadline and so do NOT hit this branch in practice;
they stamp a real ~584-year-out deadline instead. Point at the sharded `compute_expires_at`
comments (`src/stores/sharded/ttl.rs:184-186`, `src/stores/sharded/lru_ttl.rs:208-210`) as the
sharded-side source of truth instead of asserting parity. No test change needed for this half -
`cache_set_with_ttl_overflow_stores_never_expiring_entry` and
`peek_expires_at_overflowing_ttl_reports_no_deadline` already pin the single-owner behavior the
corrected comment would describe.

### 2. Decide whether to reconcile the behavior

Recommendation from this survey: reconcile toward "never expires" on the sharded side, by
treating a clamped `u64::MAX`-nanosecond TTL as a second never-expires sentinel alongside the
existing zero sentinel. Concretely:

- `src/stores/sharded/mod.rs` already has one sentinel: `decode_ttl` treats `nanos == 0` as
  "expiry disabled" (`src/stores/sharded/mod.rs:190-195`). Do NOT add a second branch there - the
  getters (`ttl()` via `ttl_duration_impl`) should keep reporting the raw configured value
  (`Some(Duration::from_nanos(u64::MAX))`), matching how `TtlCache::ttl()` keeps reporting
  `Some(Duration::MAX)` today even though the computed deadline is `None`
  (`tests/v3_single_owner_zero_ttl.rs:838-839` pins that single-owner asymmetry as intentional).
  Touching `decode_ttl` would also change `set_ttl`'s and `unset_ttl`'s "previous TTL" return
  value (`src/stores/sharded/ttl.rs:613-623`, `src/stores/sharded/lru_ttl.rs:750-761`, both call
  `decode_ttl` on the swapped-out previous nanos), which is out of scope for this reconciliation.
- The actual fix is localized to the two `compute_expires_at` methods, which are the sole
  choke point every insert and refresh-on-hit path in the sharded TTL families routes through:
  `src/stores/sharded/ttl.rs:187-195` and `src/stores/sharded/lru_ttl.rs:211-219`. Add a second
  branch: `nanos == u64::MAX` -> `None` (never expires), alongside the existing `nanos == 0` ->
  `None` branch, before the `Duration::from_nanos(nanos)` / `checked_add` call. Every call site
  of `compute_expires_at` picks this up for free, since they only ever consume its `Option`
  return: fresh inserts at `src/stores/sharded/ttl.rs:753` and `src/stores/sharded/lru_ttl.rs
  :841`, and refresh-on-hit at `src/stores/sharded/ttl.rs:664`/`1198` (both an
  `.or(entry.expires_at)` pattern) and `src/stores/sharded/lru_ttl.rs:805`/`1517`.
- No change needed to `encode_ttl` (`src/stores/sharded/mod.rs:182-184`) - it already clamps
  everything at or above `u64::MAX` nanoseconds down to the sentinel value; that clamp is what
  makes the sentinel reachable at all.
- Tests that must change if this ships: `peek_expires_at_extreme_ttl_is_clamped_not_overflowed`
  in both `src/stores/sharded/ttl.rs:3820-3839` and `src/stores/sharded/lru_ttl.rs:2910-2929`
  currently assert a real, ~584-year-out deadline comes back; they would need to assert `None`
  instead (and probably a new name, since "clamped not overflowed" would become false - the
  clamp is what NOW produces the overflow-equivalent `None`). The cross-family test
  `extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families`
  (`tests/v3_per_key_expiry_read.rs:1066-1170`) is the one built specifically to fail loudly on
  this change (see its own doc comment, `tests/v3_per_key_expiry_read.rs:1063-1065`): its sharded-
  side assertions (`tests/v3_per_key_expiry_read.rs:1112-1118`, `1128-1134`, `1157-1162`,
  `1164-1169`, all currently `deadline.is_some_and(|t| t > Instant::now() + a_century)`) would
  need to flip to `assert_eq!(deadline, None)`, and the test itself likely wants renaming (e.g.
  `extreme_ttl_agrees_across_the_whole_ttl_family`) since it would no longer be pinning a
  divergence - it would be pinning the opposite.

**Alternative: make the single-owner stores clamp too**, i.e. add the same
`.min(u64::MAX as u128)`-style clamp to `TtlCache`/`LruTtlCache`/`TtlSortedCache` so all five
stores stamp a real ~584-year deadline instead of `None`. Rejected in this survey's judgment:
it would flip the *already test-pinned* single-owner behavior
(`cache_set_with_ttl_overflow_stores_never_expiring_entry`,
`peek_expires_at_overflowing_ttl_reports_no_deadline`, and the `TtlSortedCache` equivalents), is
a more invasive change (three stores' internal representation and every call site that reasons
about "None = disabled or never-expires" on them), and moves the *friendlier* semantics (an
absurdly large TTL reads as "forever," not "a deadline nobody will ever hit but that still
occupies a spot in whatever background sweep/iteration exists") to match the *more surprising*
one. The single-owner "never expires" reading is also the one a user who deliberately writes
`Duration::MAX` most likely intends.

**Alternative: do nothing.** Leaves the divergence in place, now correctly documented (piece 1
above) and permanently pinned by design 0047's cross-family test as intentional. Cheapest option;
defensible if nobody treats this as worth a behavior change.

**This is a behavior change, not just a doc fix, if reconciliation ships.** It only affects TTLs
at or above the clamp threshold (~584 years worth of nanoseconds), so it is non-breaking for
every realistic caller. Who could observe it: (a) any caller of `cache_peek_expires_at` /
`cache_expires_at` (design 0047) on a sharded TTL store with such a TTL configured, who would see
the deadline flip from `Some(t)` to `None`; (b) any caller relying on the sharded store's TTL-
based background sweep or entry enumeration eventually reclaiming such an entry - after
reconciliation it never will, same as a zero-TTL entry today. Neither is a real-world caller for
a store that already treats "hundreds of years" as intentionally unbounded, which is the basis
for the recommendation.

## Pitfalls

- **Sentinel collision at exactly the clamp boundary.** `encode_ttl` already clamps every TTL
  whose nanosecond count is `>= u64::MAX` down to the single value `u64::MAX`
  (`src/stores/sharded/mod.rs:182-184`), so a caller who configures a TTL of *exactly*
  `Duration::from_nanos(u64::MAX)` (a genuine, literal ~584.9-year TTL, not an overflow) is
  already indistinguishable from `Duration::MAX` today. Adding the never-expires sentinel at
  that same value does not introduce a new collision - it resolves the existing one in favor of
  "never expires," which is consistent with how the single-owner stores already treat
  everything past their own overflow boundary as one bucket.
- **Don't route the sentinel through `decode_ttl`.** `decode_ttl` backs both the public `ttl()`
  getters and the "previous TTL" return values of `set_ttl`/`unset_ttl`. Special-casing
  `u64::MAX` there would make `ttl()` report `None` (indistinguishable from expiry-disabled)
  after `set_ttl(Duration::MAX)`, which is a bigger and unnecessary behavior change than the one
  being proposed, and would itself diverge from the single-owner `ttl()` getters, which keep
  reporting the raw configured value. Keep the sentinel check local to `compute_expires_at`.
  Confirm this while implementing: verify `set_ttl`'s and `unset_ttl`'s current callers don't
  already assume `decode_ttl(u64::MAX)` behaves one way or the other before touching it.
  (Verified in this survey: it currently just returns `Some(Duration::from_nanos(u64::MAX))`, no
  special case.)
- **`deep_clone` and the builder copy `ttl_nanos` verbatim** (`src/stores/sharded/ttl.rs:232`,
  `src/stores/sharded/lru_ttl.rs:259`, and the builder construction sites at
  `src/stores/sharded/ttl.rs:1111` and `src/stores/sharded/lru_ttl.rs:1330,1423`, all via
  `encode_ttl`/raw `AtomicU64::new`). None of these need to change: they only propagate the
  already-encoded nanosecond value: `compute_expires_at` is the single place that interprets it.
- **`refresh_on_hit` uses `.or(entry.expires_at)`, not a plain overwrite**
  (`src/stores/sharded/ttl.rs:664,1198`, `src/stores/sharded/lru_ttl.rs:805,1517`): once
  `compute_expires_at` returns `None` for the sentinel, a refresh hit under an extreme TTL will
  keep the entry's prior deadline if it had a finite one and the TTL was just changed to
  `Duration::MAX` (same "don't retroactively touch a stored deadline" rule design 0047 already
  pins for zero TTL, e.g. `tests/v3_per_key_expiry_read.rs` "stale deadline after ttl change"
  tests, and `src/stores/sharded/ttl.rs:3085-3086`'s comment on the disabled-TTL case). This is
  consistent with existing zero-TTL semantics on the same code path and needs no extra handling,
  but a new test pinning the extreme-TTL variant of that stale-deadline rule would close the gap
  the zero-TTL tests don't cover.
- **Don't rely on `checked_add` overflowing as the detection mechanism** on the sharded side even
  after reconciliation: `Duration::from_nanos(u64::MAX)` (~584 years) added to `Instant::now()`
  does NOT overflow `Instant`'s representable range on any of this crate's supported targets, so
  the sentinel must be an explicit `nanos == u64::MAX` check before the `checked_add`, not a
  reliance on `checked_add` returning `None`. (This is precisely why the sharded side never hit
  the overflow branch in the first place - see Current state.)

## Verification

- Read-only for this record: no source file was changed. `cargo check` was not run since no code
  changed; the file:line citations above were verified by direct reading of the cited files and
  by grepping for `Duration::MAX`, `encode_ttl`, `decode_ttl`, `ttl_nanos`, and
  `compute_expires_at` across `src/stores/ttl.rs`, `src/stores/lru_ttl.rs`,
  `src/stores/sharded/mod.rs`, `src/stores/sharded/ttl.rs`, `src/stores/sharded/lru_ttl.rs`, and
  `tests/v3_per_key_expiry_read.rs`.
- For the implementer: piece 1 (doc fix) needs no new test - run the existing suite
  (`cargo test --all-features`, per the repo's CI convention) to confirm
  `cache_set_with_ttl_overflow_stores_never_expiring_entry`,
  `peek_expires_at_overflowing_ttl_reports_no_deadline`, and
  `extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families` still pass unchanged
  (they pin behavior, not comment text).
- For piece 2 (reconciliation), if it ships: update
  `peek_expires_at_extreme_ttl_is_clamped_not_overflowed` in both `src/stores/sharded/ttl.rs` and
  `src/stores/sharded/lru_ttl.rs` and the sharded-side assertions in
  `extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families`
  (`tests/v3_per_key_expiry_read.rs:1066-1170`) as described above, add a test for the
  refresh-on-hit stale-deadline interaction under an extreme TTL (see Pitfalls), and run
  `cargo test --all-features` (mind the crate's usual `ulimit -v` guard for cache tests - a
  corrupted ring can runaway-allocate).

## Notes

- Corrections to the intake brief, for the record: (1) the brief cited
  `src/stores/ttl.rs:1134-1149` as the single-owner overflow test pin; that range is actually
  `cache_set_over_expired_keeps_the_first_stored_key`, an unrelated on_evict/key-identity test.
  The real overflow pins are `cache_set_with_ttl_overflow_stores_never_expiring_entry`
  (`src/stores/ttl.rs:1183-1198`) and `peek_expires_at_overflowing_ttl_reports_no_deadline`
  (`src/stores/ttl.rs:3012-3027`). (2) The brief stated the sharded side "reportedly has no
  `Duration::MAX` coverage." That is false: `src/stores/sharded/ttl.rs:3820-3839` and
  `src/stores/sharded/lru_ttl.rs:2910-2929` (both `peek_expires_at_extreme_ttl_is_clamped_not_
  overflowed`) already pin the clamp-to-real-deadline behavior directly, on top of the
  cross-family test in `tests/v3_per_key_expiry_read.rs`. Everything else in the brief - the two
  false doc comments, the `encode_ttl`/`decode_ttl` sentinel shape, the observable `ttl()` split,
  and the cross-family test's existence and purpose - was confirmed as reported.
- Related: 0047 (per-key expiry read), whose test suite is what makes this divergence observable
  without touching internals, and which owns the test that must change if reconciliation ships.
