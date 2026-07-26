pub mod fallback;
pub mod go;
pub mod python;
pub mod rust;
pub mod typescript;

use std::collections::HashMap;

use crate::ast::SymbolExtractor;
use crate::types::Language;

use fallback::FallbackExtractor;
use go::GoExtractor;
use python::PythonExtractor;
use rust::RustExtractor;
use typescript::TypeScriptExtractor;

pub struct LanguageRegistry {
    extractors: HashMap<Language, Box<dyn SymbolExtractor>>,
    fallback: FallbackExtractor,
}

impl LanguageRegistry {
    #[must_use]
    pub fn new() -> Self {
        let mut extractors: HashMap<Language, Box<dyn SymbolExtractor>> = HashMap::new();
        extractors.insert(Language::Rust, Box::<RustExtractor>::default());
        extractors.insert(Language::Python, Box::<PythonExtractor>::default());
        extractors.insert(Language::TypeScript, Box::<TypeScriptExtractor>::default());
        extractors.insert(Language::Go, Box::<GoExtractor>::default());

        Self {
            extractors,
            fallback: FallbackExtractor,
        }
    }

    #[must_use]
    pub fn get(&self, language: &Language) -> &dyn SymbolExtractor {
        self.extractors
            .get(language)
            .map_or(&self.fallback, AsRef::as_ref)
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
