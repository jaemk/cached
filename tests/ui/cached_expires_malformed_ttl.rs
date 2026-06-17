use cached::macros::cached;

// expires = true combined with a malformed ttl expression.
// The macro must fire the "mutually exclusive" error for expires+ttl BEFORE
// attempting to parse the ttl string.  Old code order would emit a parse error
// for the malformed ttl; new code emits the exclusion error.
#[cached(expires = true, ttl = "core::time::Duration::from_secs(")]
fn my_fn(x: u32) -> u32 {
    x
}

fn main() {}
