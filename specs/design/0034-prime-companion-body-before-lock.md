# 0034 - prime companion runs body before lock

Status: Implemented

## Current state

`#[cached]` generates a `{fn}_prime_cache` companion function alongside the memoized function.
Calling `{fn}_prime_cache(args)` unconditionally runs the body and stores the result, bypassing
the cache lookup. It is used to warm the cache before the first call or to force a refresh.

The generated prime body runs the function call before acquiring the cache lock
(`cached_proc_macro/src/cached.rs:1218-1230`):

```
// run the function first (no lock held), then cache the result
#function_call
#lock
#set_cache_and_return
```

## Design decisions recorded here

**The body runs before the lock is acquired.** The alternative (lock first, then run the body)
would deadlock any recursive cached function. `parking_lot` mutexes are non-reentrant; a cached
function whose body calls itself (or calls another cached function that shares a lock path) would
re-enter the same lock on the same thread and deadlock. Running the body outside the lock avoids
this entirely.

**Lock-before-compute also blocks readers for the full recompute duration.** The main cached
function uses a compute-then-lock pattern on the `Disabled` (no sync) path. The prime companion
mirrors this. If the lock were held during the body, every concurrent `cache_get` on the same
static would block until the prime completes, defeating the timer-driven background-refresh
pattern documented in `src/macros.rs` (MACRO-1).

**`sync_writes = "by_key"` still takes the per-key bucket lock, but only around the set.** When
`by_key` is active, the prime companion evaluates `force_refresh` (always true for a prime),
acquires the per-key bucket lock, and then sets the result. The per-key lock scope covers only
the cache write, not the body computation.

**The prime companion is not emitted under `in_impl = true`.** Under `in_impl`, the cache static
is function-local to the generated method body. A `{fn}_prime_cache` sibling cannot reach a
function-local static in a different function body, so the prime is suppressed. Spec 0036 covers
the `in_impl` static placement decision.

## Notes

- `cached_proc_macro/src/cached.rs:1218-1277` contains the prime body generation and the
  `in_impl` suppression logic.
- The comment at line 1218 ("Run the function BEFORE taking the lock") captures the rationale
  inline.
