//! Key redaction and keyring tests.

use astock_minimax::{mask_key, redact, KeyStore, SecretKey};

const KEY: &str = "sk-cp-mnB6cdef1234abcd5678";

#[test]
fn debug_and_display_are_redacted() {
    let key = SecretKey::new(KEY);
    let debug = format!("{key:?}");
    let display = format!("{key}");
    assert!(!debug.contains(KEY), "debug leaked key: {debug}");
    assert!(!display.contains(KEY), "display leaked key: {display}");
    assert!(debug.contains("sk-...5678"), "unexpected mask: {debug}");
    assert_eq!(display, "sk-...5678");
}

#[test]
fn mask_key_keeps_prefix_and_last_four() {
    assert_eq!(mask_key(KEY), "sk-...5678");
    assert_eq!(mask_key("abcd"), "****");
    assert_eq!(mask_key("xy"), "****");
    assert_eq!(mask_key("no-dash-key-material-9999"), "no-...9999");
    assert_eq!(mask_key("verylongprefixwithoutdash-9999"), "...9999");
}

#[test]
fn redact_masks_all_occurrences() {
    let log = format!("auth failed for key {KEY}; will not retry {KEY}");
    let cleaned = redact(&log, KEY);
    assert!(!cleaned.contains(KEY));
    assert_eq!(cleaned.matches("sk-...5678").count(), 2);
}

#[test]
fn redact_ignores_short_patterns_and_absent_keys() {
    // A too-short pattern would mangle unrelated text; leave it alone.
    assert_eq!(redact("abc abc", "abc"), "abc abc");
    assert_eq!(redact("nothing to hide", KEY), "nothing to hide");
}

#[test]
fn secret_key_redact_method() {
    let key = SecretKey::new(KEY);
    let cleaned = key.redact(&format!("token {KEY} expired"));
    assert!(!cleaned.contains(KEY));
    assert!(cleaned.contains("sk-...5678"));
}

#[test]
fn secret_key_never_serializes() {
    let key = SecretKey::new(KEY);
    let err = serde_json::to_string(&key).unwrap_err();
    assert!(err.to_string().contains("never be serialized"));
}

#[test]
fn keystore_roundtrip_on_custom_account() {
    // Use a throwaway account so the real app slot is never touched.
    let store = KeyStore::with_service(
        "astock-terminal-test",
        format!("minimax-test-{}", std::process::id()),
    );
    store.delete_key().ok(); // clean slate, ignore absence
    assert_eq!(store.load_key().unwrap(), None);

    let key = SecretKey::new("sk-test-key-material-0001");
    store.store_key(&key).unwrap();
    let loaded = store.load_key().unwrap().expect("key should be stored");
    assert_eq!(loaded.expose(), "sk-test-key-material-0001");

    store.delete_key().unwrap();
    assert_eq!(store.load_key().unwrap(), None);
    // Deleting twice is fine.
    store.delete_key().unwrap();
}

/// A real platform credential backend must be compiled in.
///
/// keyring 3.x silently falls back to an in-memory mock store when no platform
/// feature is enabled. In that mode `store_key` returns `Ok(())` and the value
/// is never persisted, so a user who installs a credential finds it missing
/// later and non-interactive use can never work. This crate shipped exactly
/// that on Linux and macOS, because only `windows-native` was enabled.
///
/// This test fails loudly if the workspace's keyring features are ever narrowed
/// again, on whichever platform the suite runs.
#[test]
fn a_real_platform_credential_backend_is_compiled_in() {
    let store = KeyStore::with_service(
        "astock-terminal-test",
        format!("minimax-backend-probe-{}", std::process::id()),
    );
    store.delete_key().ok();

    let key = SecretKey::new("sk-backend-probe-0002");
    // The contract under test: if the store reports success, the value must be
    // retrievable. A mock backend satisfies the first half and fails the second.
    store
        .store_key(&key)
        .expect("storing a credential must either succeed or report an error");
    let loaded = store.load_key().expect("loading must not error");
    assert!(
        loaded.is_some(),
        "the credential store accepted a key and then reported it absent, which means keyring \
         is using its in-memory mock backend; enable the platform feature for this target in \
         the workspace manifest"
    );
    assert_eq!(loaded.unwrap().expose(), "sk-backend-probe-0002");

    store.delete_key().expect("cleanup must succeed");
}
