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
    /// PHP et modèles `.phtml` : le HTML qui entoure le code est coloré lui aussi.
    Php,
    /// Markdown : blocs (titres, listes, code) et texte courant (emphase, liens, code en ligne).
    Markdown,
    Yaml,
    Xml,
    /// Scripts shell (`sh`, `bash`, `zsh`).
    Shell,
}

/// Les clés d'un mapping YAML, à placer avant les motifs génériques de la requête de coloration.
const YAML_KEYS: &str = r#"
(block_mapping_pair key: (flow_node [(double_quote_scalar) (single_quote_scalar)] @property))
(block_mapping_pair key: (flow_node (plain_scalar (string_scalar) @property)))
(flow_mapping (_ key: (flow_node [(double_quote_scalar) (single_quote_scalar)] @property)))
(flow_mapping (_ key: (flow_node (plain_scalar (string_scalar) @property))))
"#;

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
            Language::Php => "PHP",
            Language::Markdown => "Markdown",
            Language::Yaml => "YAML",
            Language::Xml => "XML",
            Language::Shell => "Shell",
        }
    }

    /// Tous les langages.
    pub const ALL: [Language; 12] = [
        Language::Rust,
        Language::Json,
        Language::Toml,
        Language::Python,
        Language::JavaScript,
        Language::Html,
        Language::Css,
        Language::Php,
        Language::Markdown,
        Language::Yaml,
        Language::Xml,
        Language::Shell,
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
            "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => Language::Php,
            "md" | "markdown" | "mdown" | "mkd" => Language::Markdown,
            "yaml" | "yml" => Language::Yaml,
            "xml" | "xsd" | "xsl" | "xslt" | "svg" | "plist" | "csproj" | "vbproj" | "props"
            | "targets" | "xaml" | "rss" | "atom" => Language::Xml,
            "sh" | "bash" | "zsh" | "ksh" => Language::Shell,
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
            Language::Php => "PHP",
            Language::Markdown => "Markdown",
            Language::Yaml => "YAML",
            Language::Xml => "XML",
            Language::Shell => "Shell",
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
            Language::Php => "php",
            Language::Markdown => "markdown",
            Language::Yaml => "yaml",
            Language::Xml => "xml",
            Language::Shell => "shellscript",
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
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Language::Xml => tree_sitter_xml::LANGUAGE_XML.into(),
            Language::Shell => tree_sitter_bash::LANGUAGE.into(),
        }
    }

    /// Requête de coloration. Celle de YAML est écrite pour un moteur où le dernier motif
    /// l'emporte ; ici c'est le premier : les clés (`@property`) sont donc placées avant le reste.
    pub(crate) fn highlights_query(self) -> std::borrow::Cow<'static, str> {
        match self {
            Language::Yaml => [YAML_KEYS, self.base_query()].join("\n").into(),
            _ => self.base_query().into(),
        }
    }

    fn base_query(self) -> &'static str {
        match self {
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Language::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Language::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Language::Python => tree_sitter_python::HIGHLIGHTS_QUERY,
            Language::JavaScript => tree_sitter_javascript::HIGHLIGHT_QUERY,
            Language::Html => tree_sitter_html::HIGHLIGHTS_QUERY,
            Language::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            Language::Php => tree_sitter_php::HIGHLIGHTS_QUERY,
            Language::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            Language::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            Language::Xml => tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            Language::Shell => tree_sitter_bash::HIGHLIGHT_QUERY,
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
        assert_eq!(l("page.phtml"), Some(Language::Php));
        assert_eq!(l("Index.PHP"), Some(Language::Php));
        assert_eq!(l("lib/util.php5"), Some(Language::Php));
        assert_eq!(l("README.md"), Some(Language::Markdown));
        assert_eq!(l("ci.YML"), Some(Language::Yaml));
        assert_eq!(l("logo.svg"), Some(Language::Xml));
        assert_eq!(l("deploy.sh"), Some(Language::Shell));
        assert_eq!(l("notes.txt"), None);
        assert_eq!(l("Makefile"), None);
    }
}
