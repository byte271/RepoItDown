use std::path::Component;

/// Compares two paths component-by-component, normalising `.` and `..`.
#[must_use]
pub fn paths_match(a: &std::path::Path, b: &std::path::Path) -> bool {
    let mut a_components = a.components();
    let mut b_components = b.components();

    loop {
        match (a_components.next(), b_components.next()) {
            (Some(a_c), Some(b_c)) => {
                if !components_equal(&a_c, &b_c) {
                    return false;
                }
            }
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// Compares two path components for equality, normalising `.` and `..`.
fn components_equal(a: &Component<'_>, b: &Component<'_>) -> bool {
    match (a, b) {
        (Component::Normal(a_str), Component::Normal(b_str)) => a_str == b_str,
        (Component::RootDir, Component::RootDir)
        | (Component::CurDir, Component::CurDir)
        | (Component::ParentDir, Component::ParentDir) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn identical_paths_match() {
        assert!(paths_match(Path::new("a/b/c.rs"), Path::new("a/b/c.rs")));
    }

    #[test]
    fn different_paths_dont_match() {
        assert!(!paths_match(Path::new("a/b.rs"), Path::new("a/c.rs")));
    }

    #[test]
    fn curdir_component_matches() {
        assert!(paths_match(Path::new("a/b.rs"), Path::new("a/b.rs")));
    }

    #[test]
    fn prefixed_paths_dont_match() {
        assert!(!paths_match(Path::new("a/b.rs"), Path::new("a/b/c.rs")));
    }
}
