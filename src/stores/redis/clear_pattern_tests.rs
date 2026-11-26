// No Redis server needed — pins the `SCAN MATCH` pattern used by `cache_clear`.
use super::{clear_match_pattern, escape_key_field, generate_redis_key};

#[test]
fn plain_segments_get_scope_and_trailing_star() {
    assert_eq!(clear_match_pattern("ns", "p"), "ns:p:*");
    // Fixed arity: an empty field still contributes its separator, so the
    // scope keeps both separators and matches the keys such a cache writes.
    assert_eq!(clear_match_pattern("", "p"), ":p:*");
    assert_eq!(clear_match_pattern("ns", ""), "ns::*");
}

#[test]
fn glob_metacharacters_in_segments_are_escaped() {
    // Unescaped, `cache[v2]` would glob-match keys outside this cache's
    // scope (e.g. `cachev:...`) and `cache_clear` would delete them.
    assert_eq!(clear_match_pattern("ns", "cache[v2]"), "ns:cache\\[v2\\]:*");
    assert_eq!(clear_match_pattern("n*s", "p?x"), "n\\*s:p\\?x:*");
    assert_eq!(clear_match_pattern("back\\slash", "p"), "back\\\\slash:p:*");
}

#[test]
fn separator_and_escape_characters_are_percent_escaped_in_the_pattern() {
    assert_eq!(clear_match_pattern("a:b", "p"), "a%3Ab:p:*");
    assert_eq!(clear_match_pattern("ns", "a:b"), "ns:a%3Ab:*");
    assert_eq!(clear_match_pattern("100%", "p"), "100%25:p:*");
}

#[test]
fn percent_escaping_and_glob_escaping_do_not_interfere() {
    // Percent-escaping runs first (it defines the literal bytes of the key
    // scope), glob-escaping second (it quotes those bytes for `SCAN MATCH`).
    // `%` is not a glob metacharacter and `\` is not percent-escaped, so the
    // two passes touch disjoint characters.
    assert_eq!(clear_match_pattern("a:b*", "p"), "a%3Ab\\*:p:*");
    assert_eq!(clear_match_pattern("ns", "[v2]:%"), "ns:\\[v2\\]%3A%25:*");
}

/// The literal part of the pattern (everything before the trailing `*`, with
/// glob escapes removed) must be exactly the scope shared by every key this
/// cache writes: `cache_clear` matches all of its own entries and nothing more.
fn assert_pattern_scopes_keys(namespace: &str, prefix: &str, key: &str) {
    fn unescape_glob(pattern: &str) -> String {
        let mut out = String::with_capacity(pattern.len());
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                out.extend(chars.next());
            } else {
                out.push(c);
            }
        }
        out
    }

    let pattern = clear_match_pattern(namespace, prefix);
    let literal = unescape_glob(
        pattern
            .strip_suffix('*')
            .expect("the clear pattern always ends with the key wildcard"),
    );
    let generated = generate_redis_key(namespace, prefix, key);
    assert!(
        generated.starts_with(&literal),
        "clear pattern {pattern:?} (literal scope {literal:?}) must cover key {generated:?}"
    );
    assert_eq!(
        &generated[literal.len()..],
        escape_key_field(key).as_ref(),
        "the wildcard must stand for exactly the escaped key field"
    );
}

#[test]
fn pattern_scope_and_generated_keys_agree() {
    assert_pattern_scopes_keys("ns", "p", "k");
    assert_pattern_scopes_keys("", "p", "k");
    assert_pattern_scopes_keys("ns", "", "k");
    assert_pattern_scopes_keys("cached-redis-store:", "p", "k");
    assert_pattern_scopes_keys("a:b", "p", "k");
    assert_pattern_scopes_keys("ns", "a:b", "k:with:colons");
    assert_pattern_scopes_keys("100%", "p%", "50%:k");
    assert_pattern_scopes_keys("n*s", "cache[v2]", "k?");
    assert_pattern_scopes_keys("back\\slash", "p", "k\\");
}

#[test]
fn pattern_no_longer_covers_a_neighbouring_cache() {
    // Before field escaping, `namespace = "a:b", prefix = "p"` scanned `a:b:p:*`,
    // which also matched every key of `namespace = "a", prefix = "b:p"`, so
    // clearing one cache silently deleted the other's entries.
    let scope = clear_match_pattern("a:b", "p");
    let literal = scope
        .strip_suffix('*')
        .expect("the clear pattern always ends with the key wildcard");
    let neighbour = generate_redis_key("a", "b:p", "k");
    assert!(
        !neighbour.starts_with(literal),
        "clear pattern {scope:?} must not cover the neighbouring key {neighbour:?}"
    );
}

#[test]
fn empty_namespace_scope_does_not_cover_a_cache_named_after_its_prefix() {
    // When an empty namespace dropped its field, `namespace = "", prefix = "p"`
    // scanned `p:*`, which covered every key of any cache with
    // `namespace = "p"`, whatever that cache's own prefix was. Under the fixed
    // arity the scope is `:p:*`, which no non-empty namespace can produce.
    let scope = clear_match_pattern("", "p");
    assert_eq!(scope, ":p:*");
    let literal = scope
        .strip_suffix('*')
        .expect("the clear pattern always ends with the key wildcard");
    for neighbour_prefix in ["q", "p", ""] {
        let neighbour = generate_redis_key("p", neighbour_prefix, "k");
        assert!(
            !neighbour.starts_with(literal),
            "clear pattern {scope:?} must not cover the key {neighbour:?} of a cache \
             whose namespace equals this cache's prefix"
        );
    }
}
