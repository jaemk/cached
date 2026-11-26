use cached::macros::cached;

struct Svc;

impl Svc {
    // The receiver is never part of the key, so the marker is redundant.
    #[cached(in_impl = true)]
    fn f(#[cached_skip] &self, id: u32) -> u32 {
        id
    }
}

fn main() {}
