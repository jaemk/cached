use super::{RedisCacheBuildError, sanitize_var_error};

#[test]
fn not_unicode_value_is_redacted() {
    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(b"redis://user:s3cret@host\xff".to_vec())
    };
    #[cfg(not(unix))]
    let raw = std::ffi::OsString::from("redis://user:s3cret@host");

    let sanitized = sanitize_var_error(std::env::VarError::NotUnicode(raw));
    let err = RedisCacheBuildError::MissingConnectionString {
        env_key: "CACHED_REDIS_CONNECTION_STRING".to_string(),
        error: sanitized,
    };
    let display = format!("{err}");
    let debug = format!("{err:?}");
    assert!(
        !display.contains("s3cret"),
        "Display leaked the value: {display}"
    );
    assert!(!debug.contains("s3cret"), "Debug leaked the value: {debug}");
    assert!(display.contains("[REDACTED connection string]"));
}

#[test]
fn not_present_is_preserved() {
    assert!(matches!(
        sanitize_var_error(std::env::VarError::NotPresent),
        std::env::VarError::NotPresent
    ));
}
