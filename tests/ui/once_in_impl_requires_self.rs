use cached::macros::once;

struct S;

// `in_impl = true` on an associated function with NO `self` receiver is rejected:
// the generated `{fn}_no_cache(args)` call inside the impl cannot resolve without
// a `Self::` qualifier, so the macro requires a `self` receiver under `in_impl`.
impl S {
    #[once(in_impl = true)]
    fn f(x: usize) -> usize {
        x
    }
}

fn main() {}
