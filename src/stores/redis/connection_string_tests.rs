// No Redis server needed -- verifies the redaction behavior of
// `ConnectionString` (returned by `connection_string()`) and that `reveal()`
// exposes the raw URL.
use super::ConnectionString;

/// `Display` of `ConnectionString` returns the redacted placeholder, not the raw URL.
#[test]
fn display_is_redacted() {
    let cs = ConnectionString("redis://:secret@127.0.0.1:6379".to_string());
    let displayed = cs.to_string();
    assert_eq!(
        displayed, "[REDACTED connection string]",
        "Display must return the redacted placeholder, got: {displayed}"
    );
    assert!(
        !displayed.contains("secret"),
        "Display must not expose the password; got: {displayed}"
    );
}

/// `Debug` of `ConnectionString` also returns the redacted placeholder.
#[test]
fn debug_is_redacted() {
    let cs = ConnectionString("redis://:secret@127.0.0.1:6379".to_string());
    let debugged = format!("{cs:?}");
    assert_eq!(
        debugged, "[REDACTED connection string]",
        "Debug must return the redacted placeholder, got: {debugged}"
    );
    assert!(
        !debugged.contains("secret"),
        "Debug must not expose the password; got: {debugged}"
    );
}

/// `reveal()` returns the raw URL, including credentials.
#[test]
fn reveal_returns_raw() {
    let raw = "redis://:secret@127.0.0.1:6379";
    let cs = ConnectionString(raw.to_string());
    assert_eq!(cs.reveal(), raw);
    assert!(cs.reveal().contains("secret"));
}

/// Both `Debug` and `Display` redact while `reveal()` still exposes the raw value.
#[test]
fn debug_and_display_redact_but_reveal_does_not() {
    let cs = ConnectionString("redis://:s3cr3t@localhost:6379/0".to_string());
    assert_eq!(cs.to_string(), "[REDACTED connection string]");
    assert_eq!(format!("{cs:?}"), "[REDACTED connection string]");
    assert!(!cs.to_string().contains("s3cr3t"));
    assert!(!format!("{cs:?}").contains("s3cr3t"));
    // The raw value is still recoverable via reveal().
    assert!(cs.reveal().contains("s3cr3t"));
}

/// `PartialEq`/`Eq`/`Hash` compare the raw unredacted URL, and `Hash` is
/// consistent with `Eq`.
#[test]
fn eq_and_hash_on_raw_url() {
    use std::hash::{DefaultHasher, Hash, Hasher};

    fn hash_of(cs: &ConnectionString) -> u64 {
        let mut hasher = DefaultHasher::new();
        cs.hash(&mut hasher);
        hasher.finish()
    }

    let a = ConnectionString("redis://:secret@127.0.0.1:6379".to_string());
    let b = ConnectionString("redis://:secret@127.0.0.1:6379".to_string());
    let c = ConnectionString("redis://:other@127.0.0.1:6379".to_string());
    assert_eq!(a, b, "same raw URL must compare equal");
    assert_ne!(a, c, "different raw URLs must compare unequal");
    assert_eq!(hash_of(&a), hash_of(&b), "Hash must be consistent with Eq");
}
