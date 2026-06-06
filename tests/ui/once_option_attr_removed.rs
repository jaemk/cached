use cached::macros::once;

#[once(option = true)]
fn find() -> Option<u64> {
    Some(0)
}

fn main() {}
