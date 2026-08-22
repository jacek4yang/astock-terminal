use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

static SENSITIVE_KEY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(api[-_ ]?key|token|secret|password|authorization|cookie|credential)")
        .expect("sensitive key regex")
});
static BEARER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._~+/=-]{6,}").expect("bearer regex"));
static ASSIGNMENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)((?:api[-_ ]?key|token|secret|password|authorization|cookie|credential)\s*[=:]\s*)[^\s,;]+",
    )
    .expect("secret assignment regex")
});

pub fn redact_text(value: &str) -> String {
    let value = BEARER.replace_all(value, "Bearer [REDACTED]");
    ASSIGNMENT
        .replace_all(&value, "${1}[REDACTED]")
        .into_owned()
}

pub fn redact_json(value: &Value) -> Value {
    redact_json_at(value, "")
}

/// Stable, one-way fingerprint for audit correlation. The original value is
/// never embedded in logs or persistence.
pub fn fingerprint_json(value: &Value) -> String {
    let canonical = serde_json::to_vec(value).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(canonical))
}

fn redact_json_at(value: &Value, key: &str) -> Value {
    if SENSITIVE_KEY.is_match(key) {
        return Value::String("[REDACTED]".to_string());
    }
    match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redact_json_at(item, "")).collect())
        }
        Value::Object(items) => Value::Object(
            items
                .iter()
                .map(|(child_key, item)| (child_key.clone(), redact_json_at(item, child_key)))
                .collect(),
        ),
        Value::String(text) => Value::String(redact_text(text)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recursively_redacts_secrets_without_hiding_market_fields() {
        let value = redact_json(&json!({
            "symbol": "300308",
            "api_key": "super-secret",
            "nested": {"authorization": "Bearer abc.def.123"},
            "message": "token=another-secret"
        }));
        let encoded = value.to_string();
        assert!(encoded.contains("300308"));
        assert!(!encoded.contains("super-secret"));
        assert!(!encoded.contains("another-secret"));
        assert!(!encoded.contains("abc.def.123"));
    }

    #[test]
    fn fingerprint_is_stable_and_never_contains_the_input() {
        let first = fingerprint_json(&json!({"symbol": "300308", "token": "secret"}));
        let second = fingerprint_json(&json!({"symbol": "300308", "token": "secret"}));
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
        assert!(!first.contains("300308"));
        assert!(!first.contains("secret"));
    }
}
