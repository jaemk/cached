use cached::macros::cached;

struct Holder<T>(T);

impl<T: Clone> Holder<T> {
    // The macro's generic guard inspects `signature.generics`, which is empty for this
    // method: `T` belongs to the enclosing `impl`, and an attribute macro applied to a
    // method receives only the method's own tokens. The guard therefore passes and the
    // generated function-local cache static names `T`, which rustc rejects with E0401.
    // Pinning the value type with `key`/`convert`/`ty` is not enough either - the static
    // still has to name a concrete value type. See design record 0036.
    #[cached(in_impl = true)]
    fn value(&self) -> T {
        self.0.clone()
    }
}

fn main() {}
