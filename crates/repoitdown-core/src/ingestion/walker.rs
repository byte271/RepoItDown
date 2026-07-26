use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};
use crate::ingestion::ignore::IgnoreFilter;
use crate::ingestion::secrets;
use crate::ingestion::{FileEntry, IngestionConfig, IngestionResult, SkippedFile};
use crate::types::Language;

pub struct RepoWalker {
    config: IngestionConfig,
}

impl RepoWalker {
    #[must_use]
    pub const fn new(config: IngestionConfig) -> Self {
        Self { config }
    }

    pub fn walk(&self, root: &Path) -> Result<IngestionResult> {
        let canonical = fs::canonicalize(root).map_err(|e| {
            Error::InvalidPath(format!("cannot resolve {}: {e}", root.display()))
        })?;

        let filter = IgnoreFilter::new(&canonical);
        let paths = filter.walk()?;
        let limit = self.config.max_file_count;
        let mut files = Vec::with_capacity(paths.len().min(limit));
        let mut skipped = Vec::new();

        for path in paths.iter().take(limit) {
            match self.process_file(path, &canonical) {
                Ok(entry) => files.push(entry),
                Err(reason) => skipped.push(SkippedFile {
                    path: path
                        .strip_prefix(&canonical)
                        .unwrap_or(path)
                        .to_path_buf(),
                    reason,
                }),
            }
        }

        let truncated_count = paths.len().saturating_sub(limit);
        let redaction_count = secrets::scan_and_redact(&mut files);

        Ok(IngestionResult::new(files, skipped, redaction_count, truncated_count))
    }

    fn process_file(&self, path: &Path, root: &Path) -> Result<FileEntry> {
        self.validate_path(path)?;

        let rel = path.strip_prefix(root).unwrap_or(path);
        let meta = fs::metadata(path)?;

        if meta.len() > self.config.max_file_size {
            return Err(Error::FileTooLarge {
                path: rel.to_path_buf(),
                size: meta.len(),
                max: self.config.max_file_size,
            });
        }

        if is_binary(path)? {
            return Err(Error::Parse {
                path: rel.to_path_buf(),
                message: "binary file".into(),
            });
        }

        let source = fs::read_to_string(path).map_err(Error::Io)?;
        let language = detect_language(path, &source);

        Ok(FileEntry::new(rel.to_path_buf(), language, source, meta.len()))
    }

    fn validate_path(&self, path: &Path) -> Result<()> {
        let depth = path.components().count();
        if depth > self.config.max_path_depth {
            return Err(Error::InvalidPath(format!(
                "path depth {depth} exceeds max {}",
                self.config.max_path_depth
            )));
        }

        if path.to_string_lossy().contains('\0') {
            return Err(Error::InvalidPath("null byte in path".into()));
        }

        Ok(())
    }
}

fn is_binary(path: &Path) -> Result<bool> {
    let mut buf = [0u8; 512];
    let mut file = fs::File::open(path)?;
    let n = file.read(&mut buf)?;

    if n == 0 {
        return Ok(false);
    }

    Ok(buf[..n].contains(&0))
}

fn detect_language(path: &Path, source: &str) -> Language {
    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
        if let Some(lang) = Language::from_extension(ext) {
            return lang;
        }
    }
    detect_shebang(source)
}

fn detect_shebang(source: &str) -> Language {
    let first = source.lines().next().unwrap_or("");
    if !first.starts_with("#!/") {
        return Language::Other(Language::UNKNOWN.into());
    }
    let last_component = first.rsplit(&['/', ' ', '\t']).next().unwrap_or("");
    match last_component {
        "python" | "python3" => Language::Python,
        "node" | "nodejs" => Language::JavaScript,
        _ => Language::Other(Language::UNKNOWN.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_by_extension() {
        assert_eq!(detect_language(Path::new("main.rs"), ""), Language::Rust);
        assert_eq!(detect_language(Path::new("app.py"), ""), Language::Python);
        assert_eq!(
            detect_language(Path::new("Makefile"), ""),
            Language::Other(Language::UNKNOWN.into())
        );
    }

    #[test]
    fn detect_shebang_python() {
        let source = "#!/usr/bin/env python3\nprint('hi')";
        assert_eq!(detect_language(Path::new("script"), source), Language::Python);
    }

    #[test]
    fn detect_shebang_node() {
        let source = "#!/usr/bin/env node\nconsole.log('hi')";
        assert_eq!(detect_language(Path::new("script"), source), Language::JavaScript);
    }

    #[test]
    fn binary_detection() {
        let tmp = std::env::temp_dir().join("repoitdown_test_binary");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("text.txt"), "hello world").unwrap();
        assert!(!is_binary(&tmp.join("text.txt")).unwrap());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn validate_path_null_byte() {
        let walker = RepoWalker::new(IngestionConfig::default());
        assert!(walker.validate_path(Path::new("foo\0bar")).is_err());
    }

    #[test]
    fn walk_collects_skipped_files() {
        let tmp = std::env::temp_dir().join("repoitdown_test_skipped");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("good.rs"), "fn main() {}").unwrap();
        fs::write(tmp.join("bad.bin"), [0u8, 1, 2, 3]).unwrap();

        let walker = RepoWalker::new(IngestionConfig::default());
        let result = walker.walk(&tmp).unwrap();

        assert!(result.files.iter().any(|f| f.path.ends_with("good.rs")));
        assert!(!result.skipped.is_empty());
        assert!(result.skipped.iter().any(|s| s.path.ends_with("bad.bin")));
        assert_eq!(result.truncated_count, 0);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn walk_truncates_when_over_limit() {
        let tmp = std::env::temp_dir().join("repoitdown_test_truncate");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for i in 0..5 {
            fs::write(tmp.join(format!("file_{i}.txt")), "content").unwrap();
        }

        let config = IngestionConfig {
            max_file_count: 3,
            ..IngestionConfig::default()
        };

        let walker = RepoWalker::new(config);
        let result = walker.walk(&tmp).unwrap();

        assert_eq!(result.files.len(), 3);
        assert_eq!(result.truncated_count, 2);

        let _ = fs::remove_dir_all(&tmp);
    }
}
