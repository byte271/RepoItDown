use crate::error::{Error, Result};

pub fn count_tokens(source: &str) -> Result<usize> {
    // each call to cl100k_base loads the BPE from embedded data; it never fails in practice
    // but we map the error to our own type to avoid a library panic
    tiktoken_rs::cl100k_base()
        .map(|bpe| bpe.encode_ordinary(source).len())
        .map_err(|e| Error::Config(format!("failed to load tokenizer: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_empty_string() {
        assert_eq!(count_tokens("").unwrap(), 0);
    }

    #[test]
    fn counts_hello_world() {
        let tokens = count_tokens("Hello, world!").unwrap();
        assert!(tokens > 0);
        assert!(tokens < 10);
    }

    #[test]
    fn counts_rust_function() {
        let tokens = count_tokens("fn main() {\n    println!(\"hello\");\n}").unwrap();
        assert!(tokens > 5);
        assert!(tokens < 30);
    }
}
