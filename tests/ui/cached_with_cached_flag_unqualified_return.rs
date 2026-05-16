use cached::macros::cached;

// A user-defined `Return<T>` that is NOT `cached::Return`. A proc macro only
// sees tokens, so the bare `Return` name passes the `with_cached_flag`
// attribute check; the mismatch then surfaces in the generated body (which
// expects `cached::Return`'s `was_cached`). This fixture pins that documented
// behavior — see the `with_cached_flag` docs on `#[cached]`.
#[derive(Clone)]
struct Return<T> {
    value: T,
}

#[cached(with_cached_flag = true)]
fn my_fn(k: i32) -> Return<i32> {
    Return { value: k }
}

fn main() {}
