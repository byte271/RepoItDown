//! Typed error handling with 11 variants covering every failure mode in the pipeline.

/// All errors the pipeline can produce.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error in {path}: {message}")]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },

    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),

    #[error("tokenizer error: {0}")]
    Tokenizer(String),

    #[error("file too large ({size} bytes, max {max}): {path}")]
    FileTooLarge {
        path: std::path::PathBuf,
        size: u64,
        max: u64,
    },

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("pipeline timed out after {elapsed:?}")]
    Timeout { elapsed: std::time::Duration },

    #[error("secret detection error in {path}: {message}")]
    SecretDetection {
        path: std::path::PathBuf,
        message: String,
    },

    #[error("token budget exceeded: used {used}, limit {limit}")]
    BudgetExceeded { used: usize, limit: usize },

    #[error("invalid source location in {path}: {message}")]
    InvalidLocation {
        path: std::path::PathBuf,
        message: String,
    },

    #[error("configuration error: {0}")]
    Config(String),
}

#[allow(
    clippy::io_other_error,
    reason = "ignore::Error does not expose a real io::ErrorKind; using Other is the closest analogue"
)]
impl From<ignore::Error> for Error {
    fn from(err: ignore::Error) -> Self {
        let msg = err.to_string();
        let io_err = err
            .into_io_error()
            .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, msg));
        Self::Io(io_err)
    }
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Io(a), Self::Io(b)) => a.kind() == b.kind(),
            (
                Self::Parse {
                    path: p1,
                    message: m1,
                },
                Self::Parse {
                    path: p2,
                    message: m2,
                },
            )
            | (
                Self::SecretDetection {
                    path: p1,
                    message: m1,
                },
                Self::SecretDetection {
                    path: p2,
                    message: m2,
                },
            )
            | (
                Self::InvalidLocation {
                    path: p1,
                    message: m1,
                },
                Self::InvalidLocation {
                    path: p2,
                    message: m2,
                },
            ) => p1 == p2 && m1 == m2,
            (
                Self::FileTooLarge {
                    path: p1,
                    size: s1,
                    max: mx1,
                },
                Self::FileTooLarge {
                    path: p2,
                    size: s2,
                    max: mx2,
                },
            ) => p1 == p2 && s1 == s2 && mx1 == mx2,
            (Self::UnsupportedLanguage(a), Self::UnsupportedLanguage(b))
            | (Self::Tokenizer(a), Self::Tokenizer(b))
            | (Self::InvalidPath(a), Self::InvalidPath(b))
            | (Self::Config(a), Self::Config(b)) => a == b,
            (Self::Timeout { elapsed: a }, Self::Timeout { elapsed: b }) => a == b,
            (
                Self::BudgetExceeded {
                    used: u1,
                    limit: l1,
                },
                Self::BudgetExceeded {
                    used: u2,
                    limit: l2,
                },
            ) => u1 == u2 && l1 == l2,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_eq_by_kind() {
        let a = Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "a"));
        let b = Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "b"));
        assert_eq!(a, b);
    }

    #[test]
    fn io_neq_by_kind() {
        let a = Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, ""));
        let b = Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "",
        ));
        assert_ne!(a, b);
    }

    #[test]
    fn different_variants_never_equal() {
        assert_ne!(
            Error::UnsupportedLanguage("zig".into()),
            Error::Tokenizer("bad".into()),
        );
    }

    #[test]
    fn io_from_conversion() {
        let err: Error = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert!(matches!(err, Error::Io(_)));
    }
}
