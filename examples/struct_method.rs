/*
Caching results of struct methods.

Two approaches are shown:

Part 1 - `in_impl = true` (headline feature): cache a method directly inside an `impl`
block. The cache static is emitted inside the method body. `self` is excluded from the
default key, so all instances share one cache. To differentiate entries per instance,
fold identifying fields into the key with a `convert` expression.

Part 2 - Free-function wrapper (portable pattern): extract the computation into a free
top-level `#[cached]` function and call it from the method, passing only the fields it
needs. Works on any Rust version and keeps the cache independent of the type.

Part 3 - Caching calls dispatched through a `dyn Trait` reference, keyed on a stable
numeric id. The trait object itself is not `Hash + Eq + Clone`, so the id serves as the
cache key and the actual computation is forwarded to a free `#[cached]` function.

Run:
    cargo run --example struct_method --features "proc_macro"
*/

use cached::macros::cached;

// ---------------------------------------------------------------------------
// Part 1 - in_impl = true with convert to fold self's id into the key
// ---------------------------------------------------------------------------
//
// The cache is process-global, not per-instance. Without `convert`, two
// `Worker` instances with the same `n` argument would share one cache entry:
// `a.compute(5)` and `b.compute(5)` would return the same value even if
// `a.factor` != `b.factor`.
//
// The fix: include `self.id` in the cache key via `convert`. Each instance
// then occupies its own key space in the shared cache.
//
// Note: because the cache static lives inside the method body, there is no
// module-level static to lock. The cache cannot be inspected or invalidated
// from outside the method (unlike the free-function pattern in basic.rs,
// where `SLOW_FN.write().remove(...)` reaches the module-level static).

struct Worker {
    /// Stable identity that distinguishes this instance in the cache.
    id: u64,
    factor: u32,
}

impl Worker {
    fn new(id: u64, factor: u32) -> Self {
        Self { id, factor }
    }

    /// Cached method. The cache static lives inside this fn body.
    /// `convert` folds `self.id` into the key so different instances
    /// do not share cache entries.
    ///
    /// A `compute_no_cache` sibling is also generated (same visibility)
    /// for calling the raw computation without touching the cache.
    /// The `_prime_cache` companion is NOT generated for `in_impl` methods.
    #[cached(in_impl = true, key = "(u64, u32)", convert = "{ (self.id, n) }")]
    fn compute(&self, n: u32) -> u32 {
        println!("  [miss] Worker(id={}) compute(n={n})", self.id);
        self.factor * n
    }
}

// ---------------------------------------------------------------------------
// Part 2 - Free function wrapping a method computation
// ---------------------------------------------------------------------------

struct Config {
    multiplier: u32,
    base: u32,
}

impl Config {
    fn new(multiplier: u32, base: u32) -> Self {
        Self { multiplier, base }
    }

    /// Calls a free cached function, forwarding only the fields it needs.
    /// The struct itself does not need to be `Hash + Eq + Clone`.
    fn compute(&self, n: u32) -> u32 {
        config_compute(self.multiplier, self.base, n)
    }
}

/// The actual cached computation lives here, as a free function.
/// Keys are derived from the plain arguments; no struct reference is stored.
#[cached]
fn config_compute(multiplier: u32, base: u32, n: u32) -> u32 {
    println!("  [miss] config_compute({multiplier}, {base}, {n})");
    multiplier * base + n
}

// ---------------------------------------------------------------------------
// Part 3 - Caching dyn-Trait calls keyed on a stable numeric id
// ---------------------------------------------------------------------------
//
// When the receiver is a `dyn Trait`, the trait object is not `Hash + Eq`.
// The workaround is the same: extract the computation into a free `#[cached]`
// function whose arguments are all `Hash + Eq + Clone` types.  A stable numeric
// id serves as the per-object cache discriminant.
//
// Here the trait exposes a factory-method-style approach: the default `process`
// implementation on the trait calls `processor_compute(self.id(), self.factor(), input)`.
// Each concrete type provides its own `factor()` - the free function handles
// the actual work and caching.
//
// Caution: `id` must be a true identity. Two objects sharing an `id` but
// differing in `factor` (or any other state that affects the result) collide
// on one cache entry, so the second object silently receives the first's
// cached value. The same caution applies to the `(self.id, n)` key in Part 1.

trait Processor {
    /// A stable id that uniquely identifies this instance across calls.
    fn id(&self) -> u64;

    /// A value derived from internal state that influences the result.
    fn factor(&self) -> u32;

    /// Public entry point - delegates to a free cached function.
    /// The cache key is (id, input); `factor` is looked up on a miss only.
    fn process(&self, input: u32) -> u32 {
        processor_compute(self.id(), self.factor(), input)
    }
}

/// Cached computation for any `Processor`-like object.
/// Key: `(id, input)`.  `factor` is only used on a cache miss.
#[cached(key = "(u64, u32)", convert = "{ (id, input) }")]
fn processor_compute(id: u64, factor: u32, input: u32) -> u32 {
    println!("  [miss] processor_compute(id={id}, factor={factor}, input={input})");
    input * factor
}

struct FastProcessor {
    id: u64,
    factor: u32,
}

impl Processor for FastProcessor {
    fn id(&self) -> u64 {
        self.id
    }
    fn factor(&self) -> u32 {
        self.factor
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

pub fn main() {
    // --- Part 1: in_impl ---
    println!("=== Part 1: in_impl = true with per-instance key ===");
    let wa = Worker::new(1, 3);
    let wb = Worker::new(2, 7);

    println!("wa.compute(5), first call (expect miss):");
    let r1 = wa.compute(5);
    println!("  result = {r1}");
    assert_eq!(r1, 3 * 5);

    println!("wa.compute(5), second call (same id -> expect hit, no [miss]):");
    let r2 = wa.compute(5);
    println!("  result = {r2}");
    assert_eq!(r1, r2);

    println!("wb.compute(5), first call (different id -> expect miss):");
    let r3 = wb.compute(5);
    println!("  result = {r3}");
    assert_eq!(r3, 7 * 5);

    println!("wb.compute(5), second call (same id -> expect hit):");
    let r4 = wb.compute(5);
    assert_eq!(r3, r4);

    // Demonstrate the generated _no_cache sibling: bypasses the cache entirely.
    println!("wa.compute_no_cache(5) bypasses cache (always recomputes):");
    let r5 = wa.compute_no_cache(5);
    println!("  result = {r5}");
    assert_eq!(r5, 3 * 5);

    // --- Part 2: free-function wrapper ---
    println!("\n=== Part 2: free-function wrapper ===");
    let cfg = Config::new(3, 10);

    println!("First call (expect miss):");
    let v1 = cfg.compute(5);
    println!("  result = {v1}");

    println!("Second call with same args (expect hit, no [miss] line):");
    let v2 = cfg.compute(5);
    println!("  result = {v2}");
    assert_eq!(v1, v2);

    println!("Call with different n=6 (expect miss):");
    let v3 = cfg.compute(6);
    println!("  result = {v3}");
    assert_eq!(v3, 3 * 10 + 6);

    // --- Part 3: dyn Trait ---
    println!("\n=== Part 3: dyn Trait keyed on stable id ===");
    let p1: &dyn Processor = &FastProcessor { id: 1, factor: 7 };
    let p2: &dyn Processor = &FastProcessor { id: 2, factor: 9 };

    println!("p1, input=4, first call (expect miss):");
    let r1 = p1.process(4);
    println!("  result = {r1}");

    println!("p1, input=4, second call (same id -> expect hit):");
    let r2 = p1.process(4);
    println!("  result = {r2}");
    assert_eq!(r1, r2);

    println!("p2, input=4, first call (different id -> expect miss):");
    let r3 = p2.process(4);
    println!("  result = {r3}");
    assert_eq!(r3, 4 * 9);

    println!("\ndone!");
}
