# Refresh-claim guard

`cached::claim::{ClaimRegistry, Claim}`: a single-flight claim on a key, so concurrent readers
that all observe the same stale entry collapse onto one refresh instead of each starting their
own. Introduced per
[design/0053-refresh-claim-guard.md](design/0053-refresh-claim-guard.md).

## CLAIM-1

`ClaimRegistry<K>` (`K: Eq + Hash + Clone`) holds the set of keys with work in flight. It is a
standalone type independent of any store: it works with `#[cached]`, `#[concurrent_cached]`, and
hand-rolled caches alike. `ClaimRegistry::new()` creates an empty registry; it is not `const`, so
a `static` registry needs `std::sync::LazyLock`.

## CLAIM-2

`ClaimRegistry::claim(key)` returns `Some(Claim<K>)` to the first caller and `None` to every
later caller while that key is claimed, until the `Claim` is dropped. `#[must_use]`, because
binding the claim is what keeps it alive for the refresh.

## CLAIM-3

`Claim<K>` releases its key from `Drop`, which runs on normal completion, on an unwind (a panic
in the refresh body), and on cancellation (an async task whose future is dropped mid-poll, e.g.
`JoinHandle::abort`). A hand-written release call at the end of the refresh body covers only the
first path; `Claim` covers all three. `Claim::key()` borrows the claimed key, so the guard cannot
be dropped before the refresh that uses it finishes without a borrow error.

## CLAIM-4

`ClaimRegistry::is_claimed(key)` reports whether a claim on `key` is currently live, accepting
any borrowed form of `K` (e.g. `&str` for `ClaimRegistry<String>`). `len()` and `is_empty()`
report the number of live claims, so tests and callers can assert the registry drains. All three
are snapshots: another thread can claim or release the key before the caller acts on the answer.

## CLAIM-5

`ClaimRegistry` is `Clone` (a cheap shared handle over one underlying set); `Claim` is not, since
two live claims on one key is the condition the type exists to prevent. Backed by
`parking_lot::Mutex`, which does not poison, so the release path in `Drop` cannot panic a second
time while a panic is already unwinding. Unconditional: no feature gate, no new dependency.

## CLAIM-6

A claim is not a lock: the registry mutex is held only across the set insert or remove, never
across the refresh itself. A caller that does not win the claim gets `None` immediately and is
never blocked. It also does not deduplicate a cold path (nothing cached yet), since the losing
callers would have no value to serve; `sync_writes = "by_key"` covers that case instead (see
[builders.md](builders.md)). The two compose and do not substitute for each other.

## CLAIM-7

`Claim`/`ClaimRegistry` are reachable through `cached::claim::` and through `cached::prelude`,
not the crate root: both are generic English words that would otherwise be offered as rustc's
nearest-match suggestion for unrelated mistyped imports, the same reasoning recorded for
`KeyedCache` (`src/lib.rs:1143-1145`).
