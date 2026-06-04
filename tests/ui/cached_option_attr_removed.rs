use cached::macros::cached;

#[cached(option = true)]
fn find(id: u64) -> Option<u64> {
    Some(id)
}

fn main() {}
