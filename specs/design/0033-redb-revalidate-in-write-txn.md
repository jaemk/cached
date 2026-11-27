# 0033 - redb re-validate-in-write-txn design

Status: Implemented

## Current state

`RedbCache` uses redb's MVCC: reads open a read transaction (snapshot), writes open a separate
write transaction. A write transaction commits atomically.

On `disk_cache_get`, when the read transaction finds an entry with a corrupt or expired value,
the store opens a write transaction to remove it. Between the read txn and the write txn, another
thread may have written a valid value for the same key via `cache_set`.

Two paths handle this:

**Expiry/refresh mutation path (`src/stores/redb.rs:942`):** re-reads the entry under the
write transaction before acting. If the key is absent, the lookup returns a miss. If the
bytes match the decoded snapshot, the lookup reuses that value. If the bytes differ, it
decodes the current entry and uses that entry to decide whether to refresh or evict.

**Self-heal path (`src/stores/redb.rs:866-911`, C5 fix):** previously deleted the key blindly
under the write transaction without re-reading. A valid `cache_set` that committed between the
read and the write was silently deleted. The self-heal path now re-reads and tries to decode the
current entry under the write transaction. It removes an entry only when the current decode
failure is eligible for self-healing.

## Design decisions recorded here

**Re-read under the write transaction is the consistent pattern.** Both the expiry/refresh path
and the self-heal path now re-read before acting. This is the correct pattern for any
read-then-conditional-write in a MVCC store.

**The refresh and expiry re-read compares raw bytes.** Matching bytes allow the lookup to
reuse its successfully decoded value. When self-healing finds a valid replacement, the lookup
carries that value and its bytes forward to the TTL check.

**A concurrent valid write is preserved.** The lookup applies TTL and refresh rules to the
current entry after revalidation.

**No additional lock beyond the write transaction.** redb's write transaction serializes all
writers, so there is at most one write transaction open at a time. The re-read and delete under
the write transaction are effectively atomic with respect to other writers.

## Notes

- Tests: `tests/v3_redb_races.rs` covers the self-heal race (corrupt-bytes fixture, concurrent
  valid `cache_set` between read and self-heal using barrier synchronization).
- The redis self-heal uses a different mechanism (Lua conditional-delete) because redis lacks
  native transaction semantics for this pattern; see spec 0029.
