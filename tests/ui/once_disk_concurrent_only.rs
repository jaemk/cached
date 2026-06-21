use cached::macros::once;

#[once(disk = true)]
fn load() -> u64 {
    42
}

fn main() {}
