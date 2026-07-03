use cached::macros::once;

// A generic `#[once]` whose value type names a function *const* parameter must
// also be rejected: the G1 guard collects const params too, and the whole-ident
// walk descends into the `[u8; N]` bracket group to find `N`. The cache static is
// monomorphic and cannot hold a value of a type that names a const parameter.
#[once]
fn generic_once_const<const N: usize>() -> [u8; N] {
    [0u8; N]
}

fn main() {}
