use cached::macros::cached;

// Under `with_cached_flag = true` the cached value type is the `Return<T>`
// wrapper (not the bare `T`): the generated code stores and clones `Return<T>`,
// so `Return<T>` - and therefore its inner `T` - must implement `Clone`.
// `cached::Return<T>` derives `Clone` with a `T: Clone` bound, so a non-`Clone`
// inner type must still produce a clear diagnostic spanned at the function's
// return type. The clone is written as `<Return<T> as Clone>::clone`, spanned
// there, so the golden pins ONE error (down from three: the assertion plus an
// E0308/E0599 cascade spanned at the attribute) and it still names the inner
// `NotClone` via the "required for `Return<NotClone>` to implement `Clone`"
// note (0043a).
struct NotClone {
    _value: u32,
}

#[cached(with_cached_flag = true)]
fn not_clone_flag_return() -> cached::Return<NotClone> {
    cached::Return::new(NotClone { _value: 0 })
}

fn main() {}
