//! Safety validation for bash commands.
//!
//! This module provides pattern-based validation to block dangerous commands
//! and flag commands that require user confirmation.

use regex::Regex;
use std::sync::LazyLock;

/// Blocked command patterns that are always rejected.
pub static BLOCKED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Destructive filesystem operations
        Regex::new(r"rm\s+(-[rfRF]+\s+)*[/~](\s|$)").unwrap(),
        Regex::new(r"rm\s+(-[rfRF]+\s+)*/\*").unwrap(),
        Regex::new(r"rm\s+(-[rfRF]+\s+)*~").unwrap(),
        // Disk/device operations
        Regex::new(r"dd\s+.*if=").unwrap(),
        Regex::new(r"mkfs").unwrap(),
        Regex::new(r">\s*/dev/sd").unwrap(),
        Regex::new(r">\s*/dev/nvme").unwrap(),
        // Permission bombs
        Regex::new(r"chmod\s+(-[rR]+\s+)*777\s+/").unwrap(),
        Regex::new(r"chown\s+(-[rR]+\s+)*.*\s+/").unwrap(),
        // Fork bomb
        Regex::new(r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;").unwrap(),
        // Dangerous redirects
        Regex::new(r">\s*/etc/").unwrap(),
        Regex::new(r">\s*/boot/").unwrap(),
        // History/config manipulation
        Regex::new(r">\s*~/\.bash").unwrap(),
        Regex::new(r">\s*~/\.profile").unwrap(),
        Regex::new(r">\s*~/\.zsh").unwrap(),
    ]
});

/// Commands that require extra caution (requires user confirmation).
pub static CAUTION_PATTERNS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        "sudo",
        "su ",
        "rm ",
        "mv ",
        "chmod",
        "chown",
        "kill",
        "pkill",
        "killall",
        "git push --force",
        "git push -f",
        "git reset --hard",
        "cargo publish",
        "npm publish",
        "docker rm",
        "docker rmi",
    ]
});

/// Check if a command matches any blocked pattern.
/// Returns the matching pattern if blocked, None if allowed.
pub fn is_blocked(command: &str) -> Option<String> {
    for pattern in BLOCKED_PATTERNS.iter() {
        if pattern.is_match(command) {
            return Some(pattern.as_str().to_string());
        }
    }
    None
}

/// Check if a command requires user confirmation.
pub fn needs_caution(command: &str) -> bool {
    CAUTION_PATTERNS
        .iter()
        .any(|pattern| command.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocked_patterns() {
        assert!(is_blocked("rm -rf /").is_some());
        assert!(is_blocked("rm -rf /*").is_some());
        assert!(is_blocked("rm -rf ~").is_some());
        assert!(is_blocked("dd if=/dev/zero of=/dev/sda").is_some());
        assert!(is_blocked("mkfs.ext4 /dev/sda1").is_some());
        assert!(is_blocked("chmod 777 /").is_some());
        assert!(is_blocked("chmod -R 777 /").is_some());
        assert!(is_blocked("chown user /").is_some());
        assert!(is_blocked(":(){ :|:& };:").is_some());
        assert!(is_blocked("echo 'malicious' > /etc/passwd").is_some());
        assert!(is_blocked("ls -l").is_none());
    }

    #[test]
    fn test_caution_patterns() {
        assert!(needs_caution("sudo apt update"));
        assert!(needs_caution("rm file.txt"));
        assert!(needs_caution("git push --force"));
        assert!(needs_caution("git reset --hard HEAD"));
        assert!(!needs_caution("ls -l"));
    }
}
