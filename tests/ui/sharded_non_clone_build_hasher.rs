// The reader `ShardHasher`'s first `#[diagnostic::on_unimplemented]` note is written for: a
// `BuildHasher` that misses one of the `Clone + Send + Sync + 'static` supertraits and so falls
// out of the blanket impl. The crate docs anticipate exactly this ("A `BuildHasher` missing any of
// those supertraits (a non-`Clone` one, say) falls outside the blanket impl and is reported as a
// missing `ShardHasher` impl"), and this golden is what pins that report.
//
// It is the case the notes must NOT send to `impl ShardHasher<u64> for SlowHasher`: that impl
// overlaps the blanket `BuildHasher` impl, so writing it yields E0277 `Clone is not satisfied` at
// the impl block and then, once `#[derive(Clone)]` is added on top of it, E0119 conflicting
// implementations. The one-step fix is `#[derive(Clone)]` alone, which the note names and rustc's
// own trailing help repeats.
//
// Distinct from `sharded_non_clone_shard_hasher.rs`: there the fixture hand-writes a `ShardHasher`
// impl and the error lands on the impl block (a supertrait violation). Here nothing is
// hand-written, the type is a plain `BuildHasher`, and the error lands at the call site as an
// unsatisfied `ShardHasher<u64>` bound -- which is where the `on_unimplemented` text fires.
use cached::ShardedUnboundCache;
use std::hash::{BuildHasher, RandomState};

// Note the missing `#[derive(Clone)]`.
struct SlowHasher(RandomState);

impl BuildHasher for SlowHasher {
    type Hasher = <RandomState as BuildHasher>::Hasher;

    fn build_hasher(&self) -> Self::Hasher {
        self.0.build_hasher()
    }
}

fn main() {
    // Stops at `hasher`: chaining `.build()` and a `set` on top only repeats the same unsatisfied
    // bound at two more spans, which would pin nothing extra.
    let _builder =
        ShardedUnboundCache::<u64, u32>::builder().hasher(SlowHasher(RandomState::new()));
}
