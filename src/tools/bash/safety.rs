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
        Regex::new(r"(?m)(^|[;&|]\s*)rm\s+(-[rfRF]+\s+)*[/~](\s|$)").unwrap(),
        Regex::new(r"(?m)(^|[;&|]\s*)rm\s+(-[rfRF]+\s+)*/\*").unwrap(),
        Regex::new(r"(?m)(^|[;&|]\s*)rm\s+(-[rfRF]+\s+)*~").unwrap(),
        // Disk/device operations
        Regex::new(r"(?m)(^|[;&|]\s*)dd\s+.*if=").unwrap(),
        Regex::new(r"(?m)(^|[;&|]\s*)mkfs").unwrap(),
        Regex::new(r">\s*/dev/sd").unwrap(),
        Regex::new(r">\s*/dev/nvme").unwrap(),
        // Permission bombs
        Regex::new(r"(?m)(^|[;&|]\s*)chmod\s+(-[rR]+\s+)*777\s+/").unwrap(),
        Regex::new(r"(?m)(^|[;&|]\s*)chown\s+(-[rR]+\s+)*.*\s+/").unwrap(),
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
pub static CAUTION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Dangerous binaries (word boundary safe)
        // Matches start of line or after command separator (; | &)
        Regex::new(r"(?m)(^|[;&|]\s*)(sudo|su|rm|mv|chmod|chown|kill|pkill|killall)(\s+|$)")
            .unwrap(),
        // Dangerous subcommands
        Regex::new(r"(?m)(^|[;&|]\s*)(docker\s+(rm|rmi)|cargo\s+publish|npm\s+publish)(\s+|$)")
            .unwrap(),
        // Dangerous flags
        Regex::new(r"git\s+push\s+.*(-f|--force)").unwrap(),
        Regex::new(r"git\s+reset\s+.*--hard").unwrap(),
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
        .any(|pattern| pattern.is_match(command))
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
    fn test_blocked_false_positives() {
        assert!(
            is_blocked("echo harm -rf /").is_none(),
            "Should not block 'harm'"
        );
        assert!(
            is_blocked("echo farm -rf /*").is_none(),
            "Should not block 'farm'"
        );
    }

    #[test]
    fn test_caution_patterns() {
        assert!(needs_caution("sudo apt update"));
        assert!(needs_caution("rm file.txt"));
        assert!(needs_caution("git push --force"));
        assert!(needs_caution("git reset --hard HEAD"));
        assert!(!needs_caution("ls -l"));
    }

    #[test]
    fn test_caution_patterns_edge_cases() {
        assert!(needs_caution("rm\tfile.txt"), "Should catch tab separator");
        assert!(
            needs_caution("rm\nfile.txt"),
            "Should catch newline separator"
        );
        assert!(
            needs_caution("echo hi; rm file"),
            "Should catch rm after semicolon"
        );
        assert!(
            needs_caution("echo hi | rm file"),
            "Should catch rm after pipe"
        );
        assert!(needs_caution("rm"), "Should catch bare rm");
    }

    #[test]
    fn test_caution_false_positives() {
        assert!(
            !needs_caution("echo farm animal"),
            "Should not flag 'farm' as dangerous"
        );
        assert!(
            !needs_caution("echo supper"),
            "Should not flag 'supper' (contains 'su')"
        );
        assert!(
            !needs_caution("mkdir form"),
            "Should not flag 'form' (matches 'rm' substring)"
        );
        assert!(!needs_caution("echo remove"), "Should not flag 'remove'");
    }
}
