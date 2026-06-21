use cached::macros::cached;

#[cached(disk = true)]
fn load(id: u64) -> u64 {
    id
}

fn main() {}
