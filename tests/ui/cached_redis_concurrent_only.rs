use cached::macros::cached;

#[cached(redis = true)]
fn load(id: u64) -> u64 {
    id
}

fn main() {}
