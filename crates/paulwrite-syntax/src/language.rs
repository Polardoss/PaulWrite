//! Langages pris en charge et association aux extensions de fichier.

use std::path::Path;

/// Langage de programmation / de données reconnu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Json,
    Toml,
    Python,
    JavaScript,
    Html,
    Css,
}

impl Language {
    /// Nom affiché dans la barre d'état.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Json => "JSON",
            Language::Toml => "TOML",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::Html => "HTML",
            Language::Css => "CSS",
        }
    }

    /// Tous les langages.
    pub const ALL: [Language; 7] = [
        Language::Rust,
        Language::Json,
        Language::Toml,
        Language::Python,
        Language::JavaScript,
        Language::Html,
        Language::Css,
    ];

    /// Langage d'après l'extension du fichier (insensible à la casse).
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Language::Rust,
            "json" => Language::Json,
            "toml" => Language::Toml,
            "py" | "pyw" => Language::Python,
            "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
            "html" | "htm" => Language::Html,
            "css" => Language::Css,
            _ => return None,
        })
    }

    /// Nom affichable.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Json => "JSON",
            Language::Toml => "TOML",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::Html => "HTML",
            Language::Css => "CSS",
        }
    }

    /// Identifiant de langage du protocole LSP (`languageId`).
    #[must_use]
    pub fn lsp_id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Json => "json",
            Language::Toml => "toml",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::Html => "html",
            Language::Css => "css",
        }
    }

    pub(crate) fn grammar(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            Language::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Html => tree_sitter_html::LANGUAGE.into(),
            Language::Css => tree_sitter_css::LANGUAGE.into(),
        }
    }

    pub(crate) fn highlights_query(self) -> &'static str {
        match self {
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Language::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Language::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Language::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Language::JavaScript => tree_sitter_javascript::HIGHLIGHT_QUERY,
            Language::Html => tree_sitter_html::HIGHLIGHTS_QUERY,
            Language::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        let l = |p: &str| Language::from_path(Path::new(p));
        assert_eq!(l("src/main.rs"), Some(Language::Rust));
        assert_eq!(l("A.JSON"), Some(Language::Json));
        assert_eq!(l("x.tar.py"), Some(Language::Python));
        assert_eq!(l("index.htm"), Some(Language::Html));
        assert_eq!(l("notes.txt"), None);
        assert_eq!(l("Makefile"), None);
    }
}
