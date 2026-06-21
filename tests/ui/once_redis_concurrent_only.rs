use cached::macros::once;

#[once(redis = true)]
fn load() -> u64 {
    42
}

fn main() {}
