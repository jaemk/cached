use cached::macros::cached;

// Under `with_cached_flag = true` the cached value type is the `Return<T>`
// wrapper (not the bare `T`): the generated code stores `Return<T>` and
// `.clone()`s/`.to_owned()`s it, so `Return<T>` - and therefore its inner
// `T` - must implement `Clone`. `cached::Return<T>` derives `Clone` with a
// `T: Clone` bound, so a non-`Clone` inner type must still produce a clear
// diagnostic spanned at the function's return type via the return-type
// `Clone` assertion, rather than an opaque error inside macro internals.
struct NotClone {
    _value: u32,
}

#[cached(with_cached_flag = true)]
fn not_clone_flag_return() -> cached::Return<NotClone> {
    cached::Return::new(NotClone { _value: 0 })
}

fn main() {}
