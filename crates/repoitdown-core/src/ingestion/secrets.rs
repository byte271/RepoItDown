use regex::Regex;
use std::sync::LazyLock;

use crate::ingestion::FileEntry;

static SECRET_PATTERNS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    [
        (r"sk-(?:proj-)?[A-Za-z0-9]{20,}", "OpenAI API key"),
        (r"gh[pous]_[A-Za-z0-9]{36,}", "GitHub token"),
        (r"AKIA[0-9A-Z]{16}", "AWS access key"),
        (r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9._-]{10,}", "JWT token"),
        (r"xox[bprs]-[A-Za-z0-9-]{10,}", "Slack token"),
        (r"sk_live_[A-Za-z0-9]{20,}", "Stripe live key"),
        (r"pk_live_[A-Za-z0-9]{20,}", "Stripe publishable key"),
        (r"-----BEGIN (?:RSA |EC |DSA )?PRIVATE KEY-----", "private key"),
        (r"(?:postgres|mysql|mongodb)://[^:\s]+:[^@\s]+@", "connection string"),
    ]
    .iter()
    .map(|(p, label)| (Regex::new(p).expect("built-in secret regex failed to compile"), *label))
    .collect()
});

#[derive(Debug, Clone)]
pub struct Redaction {
    pub label: &'static str,
    pub start: usize,
    pub end: usize,
}

#[must_use]
pub fn scan_and_redact(entries: &mut [FileEntry]) -> usize {
    let mut count = 0usize;
    for entry in entries.iter_mut() {
        let redactions = find_redactions(&entry.source);
        if !redactions.is_empty() {
            entry.has_redactions = true;
            entry.source = apply_redactions(&entry.source, &redactions);
            count += redactions.len();
        }
    }
    count
}

fn find_redactions(source: &str) -> Vec<Redaction> {
    let mut results = Vec::new();

    for (regex, label) in SECRET_PATTERNS.iter() {
        for m in regex.find_iter(source) {
            let (start, end) = (m.start(), m.end());
            if results.iter().all(|r: &Redaction| r.end <= start || r.start >= end) {
                results.push(Redaction { label, start, end });
            }
        }
    }

    let mut pos = 0usize;
    while pos < source.len() {
        let Some(window_end) = source[pos..].char_indices().nth(20).map(|(i, _)| pos + i) else {
            break;
        };
        let window = &source[pos..window_end];
        if shannon_entropy(window) >= 4.0
            && has_diverse_chars(window)
            && results
                .iter()
                .all(|r: &Redaction| r.end <= pos || r.start >= window_end)
        {
            results.push(Redaction {
                label: "high-entropy string",
                start: pos,
                end: window_end,
            });
        }
        pos += source[pos..].chars().next().map_or(1, char::len_utf8);
    }

    results.sort_by_key(|r| r.start);
    results
}

fn apply_redactions(source: &str, redactions: &[Redaction]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;

    for r in redactions {
        out.push_str(&source[cursor..r.start]);
        write!(out, "[REDACTED: {}]", r.label).ok();
        cursor = r.end;
    }
    out.push_str(&source[cursor..]);
    out
}

fn shannon_entropy(s: &str) -> f64 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }

    let mut counts = [0u32; 256];
    for &b in bytes {
        counts[usize::from(b)] += 1;
    }

    #[allow(clippy::cast_precision_loss)]
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            #[allow(clippy::cast_precision_loss)]
            let p = f64::from(c) / len;
            -p * p.log2()
        })
        .sum()
}

fn has_diverse_chars(s: &str) -> bool {
    let mut bits = 0u8;
    for ch in s.chars() {
        match ch {
            'a'..='z' => bits |= 1,
            'A'..='Z' => bits |= 2,
            '0'..='9' => bits |= 4,
            _ => bits |= 8,
        }
        if bits.count_ones() >= 3 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_openai_key() {
        let source = "const KEY = \"sk-abc123def456ghi789jkl012mno345pqr678stu\";";
        let redactions = find_redactions(source);
        assert!(!redactions.is_empty());
        assert_eq!(redactions[0].label, "OpenAI API key");
    }

    #[test]
    fn detect_github_token() {
        let source = "token: ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let redactions = find_redactions(source);
        assert!(!redactions.is_empty());
        assert_eq!(redactions[0].label, "GitHub token");
    }

    #[test]
    fn detect_jwt() {
        let source = "auth: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3pHNDLhNsPx_KU";
        let redactions = find_redactions(source);
        assert!(!redactions.is_empty());
        assert_eq!(redactions[0].label, "JWT token");
    }

    #[test]
    fn high_entropy_string_detected() {
        let source = "api_key = \"Kx9#mP2@vL5!nQ8$wR3&tY6^bF1\";";
        let redactions = find_redactions(source);
        assert!(!redactions.is_empty());
    }

    #[test]
    fn normal_text_not_flagged() {
        let source = "fn main() { println!(\"Hello, world!\"); }";
        let redactions = find_redactions(source);
        assert!(redactions.is_empty());
    }

    #[test]
    fn apply_redactions_replaces_text() {
        let source = "key=sk-abc123def456ghi789jkl012mno345pqr678stu";
        let redactions = find_redactions(source);
        let redacted = apply_redactions(source, &redactions);
        assert!(redacted.contains("[REDACTED: OpenAI API key]"));
        assert!(!redacted.contains("sk-abc123"));
    }

    #[test]
    fn scan_and_redact_modifies_entries() {
        let mut entries = vec![FileEntry::new(
            std::path::PathBuf::from("test.txt"),
            crate::types::Language::Other("text".into()),
            "secret=sk-abc123def456ghi789jkl012mno345pqr678stu".into(),
            100,
        )];
        let count = scan_and_redact(&mut entries);
        assert_eq!(count, 1);
        assert!(entries[0].has_redactions);
        assert!(entries[0].source.contains("[REDACTED: OpenAI API key]"));
    }
}
