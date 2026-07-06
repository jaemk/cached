# 0033 - redb re-validate-in-write-txn design

Status: Implemented

## Current state

`RedbCache` uses redb's MVCC: reads open a read transaction (snapshot), writes open a separate
write transaction. A write transaction commits atomically.

On `disk_cache_get`, when the read transaction finds an entry with a corrupt or expired value,
the store opens a write transaction to remove it. Between the read txn and the write txn, another
thread may have written a valid value for the same key via `cache_set`.

Two paths handle this:

**Expiry/refresh mutation path (`src/stores/redb.rs:920-936`):** re-reads the entry under the
write transaction before acting. If the key is now absent or has a fresh value, the stale action
is skipped. This was the original design.

**Self-heal path (`src/stores/redb.rs:808-867`, C5 fix):** previously deleted the key blindly
under the write transaction without re-reading. A valid `cache_set` that committed between the
read and the write was silently deleted. The fix aligns the self-heal path with the expiry path:
re-read the bytes under the write transaction; only delete if the bytes are still corrupt (same
raw bytes as the read txn observed). A concurrent valid write has different bytes, so it is
preserved.

## Design decisions recorded here

**Re-read under the write transaction is the consistent pattern.** Both the expiry/refresh path
and the self-heal path now re-read before acting. This is the correct pattern for any
read-then-conditional-write in a MVCC store.

**The re-read compares raw bytes, not deserialized values.** Deserialization is skipped on the
self-heal re-read; the bytes are compared directly. If the bytes differ, a concurrent writer has
updated the entry and the self-heal is aborted. If the bytes are identical, the corruption is
persistent and the delete proceeds.

**A concurrent valid write is never lost.** The worst case after the fix is a spurious cache miss
(the self-heal aborts and returns `Ok(None)` even though a valid entry now exists). The next
`cache_get` will read the valid entry. Losing a valid write entirely was the bug being fixed.

**No additional lock beyond the write transaction.** redb's write transaction serializes all
writers, so there is at most one write transaction open at a time. The re-read and delete under
the write transaction are effectively atomic with respect to other writers.

## Notes

- Tests: `tests/v3_redb_races.rs` covers the self-heal race (corrupt-bytes fixture, concurrent
  valid `cache_set` between read and self-heal using barrier synchronization).
- The redis self-heal uses a different mechanism (Lua conditional-delete) because redis lacks
  native transaction semantics for this pattern; see spec 0029.
