//! Image resolution and session name validation.

/// Resolve user input to a full image reference.
///
/// - Contains `/` or `:` → verbatim
/// - Otherwise → `docker.io/library/{input}:latest`
pub fn resolve(input: &str) -> String {
    if input.contains('/') || input.contains(':') {
        input.to_string()
    } else {
        format!("docker.io/library/{input}:latest")
    }
}

/// Validate a session name: non-empty, alphanumeric + hyphens + underscores.
pub fn is_valid_session_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_bare_name() {
        assert_eq!(resolve("ubuntu"), "docker.io/library/ubuntu:latest");
        assert_eq!(resolve("debian"), "docker.io/library/debian:latest");
        assert_eq!(resolve("node"), "docker.io/library/node:latest");
    }

    #[test]
    fn resolve_with_tag() {
        assert_eq!(resolve("node:22"), "node:22");
        assert_eq!(resolve("ubuntu:24.04"), "ubuntu:24.04");
    }

    #[test]
    fn resolve_with_registry() {
        assert_eq!(resolve("ghcr.io/org/img:tag"), "ghcr.io/org/img:tag");
        assert_eq!(
            resolve("docker.io/library/ubuntu"),
            "docker.io/library/ubuntu"
        );
    }

    #[test]
    fn valid_session_names() {
        assert!(is_valid_session_name("my-session"));
        assert!(is_valid_session_name("test_123"));
        assert!(is_valid_session_name("a"));
        assert!(is_valid_session_name("AbC-123_def"));
    }

    #[test]
    fn invalid_session_names() {
        assert!(!is_valid_session_name(""));
        assert!(!is_valid_session_name("has space"));
        assert!(!is_valid_session_name("has.dot"));
        assert!(!is_valid_session_name("path/slash"));
        assert!(!is_valid_session_name("semi;colon"));
    }
}
