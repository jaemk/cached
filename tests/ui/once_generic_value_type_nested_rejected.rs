use cached::macros::once;

// A generic `#[once]` whose value type names a function type parameter *nested*
// inside another generic (`Vec<T>`) must still be rejected. The whole-ident walk
// descends through the value type's token stream, so the `T` inside `Vec<T>` is
// caught even though `"Vec<T>"` is not literally equal to `"T"`. This is the case
// the substring->whole-ident fix must keep catching: a genuine, non-substring,
// whole-ident match on the param.
#[once]
fn generic_once_nested<T: Clone>(x: T) -> Vec<T> {
    vec![x]
}

fn main() {}
