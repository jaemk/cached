use cached::macros::once;

// A generic `#[once]` whose return type names a function type parameter is
// rejected: the cache static is monomorphic and cannot hold a value of type `T`.
// A non-generic `#[once]` and a generic `#[once]` with a concrete return type
// (`fn foo<T>(_: T) -> usize`) must still compile.
#[once]
fn generic_once<T: Clone>(x: T) -> T {
    x
}

fn main() {}
