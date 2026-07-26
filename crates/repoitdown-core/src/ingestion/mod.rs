pub mod ignore;
pub mod secrets;
pub mod walker;

use std::path::PathBuf;

use crate::error::Error;
use crate::types::Language;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub language: Language,
    pub source: String,
    pub size_bytes: u64,
    pub has_redactions: bool,
}

impl FileEntry {
    #[must_use]
    pub const fn new(path: PathBuf, language: Language, source: String, size_bytes: u64) -> Self {
        Self {
            path,
            language,
            source,
            size_bytes,
            has_redactions: false,
        }
    }
}

#[derive(Debug)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: Error,
}

#[derive(Debug, Clone)]
pub struct IngestionConfig {
    pub max_file_size: u64,
    pub max_file_count: usize,
    pub max_path_depth: usize,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            max_file_size: 1_048_576,
            max_file_count: 10_000,
            max_path_depth: 64,
        }
    }
}

#[derive(Debug)]
pub struct IngestionResult {
    pub files: Vec<FileEntry>,
    pub skipped: Vec<SkippedFile>,
    pub redaction_count: usize,
    pub truncated_count: usize,
}

impl IngestionResult {
    #[must_use]
    pub const fn new(
        files: Vec<FileEntry>,
        skipped: Vec<SkippedFile>,
        redaction_count: usize,
        truncated_count: usize,
    ) -> Self {
        Self {
            files,
            skipped,
            redaction_count,
            truncated_count,
        }
    }
}
