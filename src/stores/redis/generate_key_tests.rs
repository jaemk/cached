// No Redis server needed: pins the data-impacting key format (1.0 §8, field
// escaping in 3.0).
use super::{DEFAULT_NAMESPACE, generate_redis_key};

#[test]
fn default_namespace_trailing_colon_trimmed_and_rejoined() {
    // The canonical §8 example.
    assert_eq!(
        generate_redis_key(DEFAULT_NAMESPACE, "my_prefix", "my_key"),
        "cached-redis-store:my_prefix:my_key"
    );
    // DEFAULT_NAMESPACE itself ends in ':' — only trailing colons are trimmed.
    assert!(DEFAULT_NAMESPACE.ends_with(':'));
}

#[test]
fn empty_fields_keep_their_separators() {
    // Fixed arity: every key carries exactly two separators, so an empty
    // field is an empty field rather than a missing one. Dropping the
    // separator of an empty field would make `("", "p", "k")` and
    // `("p", "", "k")` collide, and would let a namespace-less cache's clear
    // scope reach a cache whose namespace equals its prefix.
    assert_eq!(generate_redis_key("", "p", "k"), ":p:k");
    assert_eq!(generate_redis_key("ns", "", "k"), "ns::k");
    assert_eq!(generate_redis_key("", "", "k"), "::k");
    assert_eq!(generate_redis_key(":", "", "k"), "::k"); // ":" trims to empty
}

#[test]
fn full_form_and_multiple_trailing_colons() {
    assert_eq!(generate_redis_key("ns", "p", "k"), "ns:p:k");
    // `canonical_namespace` strips *all* trailing colons.
    assert_eq!(generate_redis_key("ns:::", "p", "k"), "ns:p:k");
}

#[test]
fn interior_separators_and_escape_characters_are_escaped() {
    assert_eq!(generate_redis_key("a:b", "p", "k"), "a%3Ab:p:k");
    assert_eq!(generate_redis_key("ns", "a:b", "k"), "ns:a%3Ab:k");
    assert_eq!(generate_redis_key("ns", "p", "a:b"), "ns:p:a%3Ab");
    // The escape character itself is escaped, so an already-percent-looking
    // field cannot be confused with an escaped separator.
    assert_eq!(generate_redis_key("a%3Ab", "p", "k"), "a%253Ab:p:k");
    assert_eq!(generate_redis_key("100%", "p", "k"), "100%25:p:k");
}

#[test]
fn interior_separators_no_longer_collide() {
    // A namespace containing an interior separator used to produce the same
    // key as a shorter namespace carrying the remainder as its prefix.
    let with_interior = generate_redis_key("ns:evil", "", "k");
    let split_across = generate_redis_key("ns", "evil", "k");
    assert_ne!(
        with_interior, split_across,
        "an interior separator must not alias a differently-split namespace/prefix pair"
    );
    assert_eq!(with_interior, "ns%3Aevil::k");
    assert_eq!(split_across, "ns:evil:k");
}

/// Distinct `(canonical namespace, prefix, key)` triples must map to distinct
/// keys. The triples below are pairwise distinct after namespace
/// canonicalization and include every splitting of the same characters
/// across the three fields that previously aliased.
#[test]
fn distinct_triples_map_to_distinct_keys() {
    let triples = [
        ("", "p", "k"),
        ("p", "", "k"),
        ("", "", "p:k"),
        ("a", "b", "c"),
        ("a:b", "c", "d"),
        ("a", "b:c", "d"),
        ("a", "b", "c:d"),
        ("a:b:c", "", "d"),
        ("a%3Ab", "c", "d"),
        ("a", "%", "d"),
        ("a", "%25", "d"),
        ("ns", "p", ""),
        ("ns", "", "p"),
    ];
    let mut seen = std::collections::HashMap::new();
    for (namespace, prefix, key) in triples {
        let generated = generate_redis_key(namespace, prefix, key);
        if let Some(previous) = seen.insert(generated.clone(), (namespace, prefix, key)) {
            panic!(
                "{:?} and {:?} both map to {generated:?}",
                previous,
                (namespace, prefix, key)
            );
        }
    }
}

/// Every field is recoverable from the generated key, which is the property
/// that makes the mapping injective: escaping removes `:` from the namespace
/// and prefix fields, and the arity is fixed, so the first two `:` in a key
/// are always its separators.
#[test]
fn every_field_is_recoverable_from_the_key() {
    fn unescape_field(field: &str) -> String {
        field.replace("%3A", ":").replace("%25", "%")
    }
    fn decode(generated: &str) -> (String, String, String) {
        let fields: Vec<&str> = generated.splitn(3, ':').collect();
        match fields.as_slice() {
            [namespace, prefix, key] => (
                unescape_field(namespace),
                unescape_field(prefix),
                unescape_field(key),
            ),
            other => panic!("unexpected key layout: {other:?}"),
        }
    }

    for (namespace, prefix, key) in [
        ("ns", "p", "k"),
        ("", "p", "k"),
        ("ns", "", "k"),
        ("", "", ""),
        ("a:b", "c:d", "e:f"),
        ("100%", "%3A", "%25"),
        ("cached-redis-store", "my_prefix", "my_key"),
    ] {
        let generated = generate_redis_key(namespace, prefix, key);
        assert_eq!(
            decode(&generated),
            (namespace.to_string(), prefix.to_string(), key.to_string()),
            "key {generated:?} must decode back to its fields"
        );
    }
}
