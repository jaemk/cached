use cached::macros::cached;

// The marker is bare; it takes no arguments.
#[cached]
fn f(id: u32, #[cached_skip(always)] tag: String) -> u32 {
    let _ = tag;
    id
}

fn main() {}
