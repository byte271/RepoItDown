use ignore::WalkBuilder;

use crate::error::Result;

#[derive(Debug)]
pub struct IgnoreFilter {
    root: std::path::PathBuf,
}

impl IgnoreFilter {
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn walk(&self) -> Result<Vec<std::path::PathBuf>> {
        let mut paths = Vec::new();
        for result in WalkBuilder::new(&self.root)
            .standard_filters(true)
            .hidden(false)
            .build()
        {
            let entry = result?;
            if entry.file_type().is_some_and(|ft| ft.is_file()) {
                paths.push(entry.into_path());
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_ignores_git_dir() {
        let tmp = std::env::temp_dir().join("repoitdown_test_ignore");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src/main.rs"), "fn main() {}").unwrap();

        let filter = IgnoreFilter::new(&tmp);
        let paths = filter.walk().unwrap();
        let names: Vec<&str> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();

        assert!(names.contains(&"main.rs"));
        assert!(!names.contains(&".git"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn filter_respects_gitignore() {
        let tmp = std::env::temp_dir().join("repoitdown_test_ignorefile");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(tmp.join("secret.txt"), "secret").unwrap();
        std::fs::write(tmp.join(".ignore"), "*.txt\n").unwrap();

        let filter = IgnoreFilter::new(&tmp);
        let paths = filter.walk().unwrap();
        let names: Vec<&str> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();

        assert!(names.contains(&"main.rs"));
        assert!(!names.contains(&"secret.txt"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
