//! Single choke point for text that leaves the crate.
//!
//! Every provider-supplied string (HTTP error bodies, transport messages,
//! malformed stream frames) passes through [`scrub`] before it reaches an
//! error value, a `Display` implementation, or a tracing event. Provider error
//! bodies routinely echo a prefix of the presented credential, so the resolved
//! key is substituted out unconditionally rather than only when a heuristic
//! thinks a secret is present.

use secrecy::{ExposeSecret as _, SecretString};

/// Replacement written wherever key material would otherwise appear.
pub(crate) const REDACTED: &str = "[redacted]";

/// Upper bound on any provider-controlled string retained in an error.
const MAX_RETAINED: usize = 512;

/// Remove key material from provider-controlled text and bound its length.
pub(crate) fn scrub(text: &str, key: Option<&SecretString>) -> String {
    let mut cleaned = match key {
        // A short "secret" cannot be matched safely: replacing a 1-3 character
        // needle would corrupt unrelated text without protecting anything.
        Some(key) if key.expose_secret().len() >= 4 => text.replace(key.expose_secret(), REDACTED),
        _ => text.to_owned(),
    };
    if cleaned.chars().count() > MAX_RETAINED {
        cleaned = cleaned.chars().take(MAX_RETAINED).collect::<String>() + "…";
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, scrub};
    use secrecy::SecretString;

    #[test]
    fn scrub_removes_every_occurrence_of_the_key() {
        let key = SecretString::from("fixture-provider-token-1234");
        let text = "invalid key fixture-provider-token-1234 (fixture-provider-token-1234)";
        let cleaned = scrub(text, Some(&key));
        assert!(!cleaned.contains("fixture-provider-token-1234"));
        assert_eq!(cleaned.matches(REDACTED).count(), 2);
    }

    #[test]
    fn scrub_bounds_provider_controlled_length() {
        let cleaned = scrub(&"a".repeat(4096), None);
        assert_eq!(cleaned.chars().count(), 513);
    }

    #[test]
    fn scrub_leaves_text_alone_for_implausibly_short_keys() {
        let key = SecretString::from("ab");
        assert_eq!(scrub("about", Some(&key)), "about");
    }
}
