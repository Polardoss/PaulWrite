//! Configuration utilisateur : fichier `config.toml` dans `%APPDATA%\PaulWrite`.
//!
//! Format : sous-ensemble minimal de TOML (`clé = valeur`, commentaires `#`, entiers, booléens et
//! chaînes). Un analyseur TOML complet (crate `toml`) sera ajouté avec les thèmes (étape 6) ;
//! d'ici là, une dépendance de plus pour trois réglages n'est pas justifiée (règle n°7).
//!
//! Le fichier est lu **sur le worker**, jamais sur le thread UI.

use std::path::PathBuf;

/// Taille maximale (octets) d'un fichier coloré par défaut. Au-delà, la coloration est désactivée
/// (voir `docs/benchmarks.md` pour les mesures qui fondent cette valeur).
pub const DEFAULT_HIGHLIGHT_MAX_BYTES: usize = 1024 * 1024;

/// Taille maximale (octets) d'un fichier affiché avec retour à la ligne par défaut : au-delà, les
/// lignes ne sont pas coupées (voir `docs/benchmarks.md`).
pub const DEFAULT_WRAP_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Réglages de l'application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Retour à la ligne automatique au démarrage (`word_wrap = false` pour le désactiver).
    pub word_wrap: bool,
    /// Au-delà de cette taille, pas de retour à la ligne (`word_wrap_max_bytes`).
    pub word_wrap_max_bytes: usize,
    /// Au-delà de cette taille, pas de coloration syntaxique (`highlight_max_bytes`).
    pub highlight_max_bytes: usize,
    /// Colorer ou non (`highlight = false` la désactive partout).
    pub highlight: bool,
    /// Serveurs de langage (diagnostics, complétion, définition) : désactivés par défaut, car ils
    /// lancent des programmes externes. `lsp = true` les active.
    pub lsp: bool,
    /// Commande du serveur de chaque langage, par identifiant LSP : `lsp_rust = "rust-analyzer"`.
    pub lsp_servers: std::collections::HashMap<String, String>,
}

/// Serveurs de langage proposés par défaut (à installer soi-même ; utilisés si `lsp = true`).
const DEFAULT_SERVERS: [(&str, &str); 7] = [
    ("rust", "rust-analyzer"),
    ("python", "pylsp"),
    ("javascript", "typescript-language-server --stdio"),
    ("json", "vscode-json-language-server --stdio"),
    ("html", "vscode-html-language-server --stdio"),
    ("css", "vscode-css-language-server --stdio"),
    ("toml", "taplo lsp stdio"),
];

impl Default for Config {
    fn default() -> Self {
        Self {
            word_wrap: true,
            word_wrap_max_bytes: DEFAULT_WRAP_MAX_BYTES,
            highlight_max_bytes: DEFAULT_HIGHLIGHT_MAX_BYTES,
            highlight: true,
            lsp: false,
            lsp_servers: std::collections::HashMap::new(),
        }
    }
}

impl Config {
    /// Analyse le contenu d'un fichier de configuration. Les clés inconnues et les lignes
    /// invalides sont ignorées (un réglage erroné ne doit pas empêcher l'éditeur de démarrer).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            match key {
                "highlight_max_bytes" => {
                    if let Ok(n) = value.replace('_', "").parse::<usize>() {
                        c.highlight_max_bytes = n;
                    }
                }
                "highlight" => {
                    if let Ok(b) = value.parse::<bool>() {
                        c.highlight = b;
                    }
                }
                "word_wrap" => {
                    if let Ok(b) = value.parse::<bool>() {
                        c.word_wrap = b;
                    }
                }
                "word_wrap_max_bytes" => {
                    if let Ok(n) = value.replace('_', "").parse::<usize>() {
                        c.word_wrap_max_bytes = n;
                    }
                }
                "lsp" => {
                    if let Ok(b) = value.parse::<bool>() {
                        c.lsp = b;
                    }
                }
                k if k.starts_with("lsp_") && !value.is_empty() => {
                    c.lsp_servers.insert(k[4..].to_owned(), value.to_owned());
                }
                _ => {}
            }
        }
        c
    }

    /// Emplacement du fichier de configuration.
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("PaulWrite").join("config.toml"))
    }

    /// Dossier des plugins Lua : `%APPDATA%\\PaulWrite\\plugins`.
    #[must_use]
    pub fn plugins_dir() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("PaulWrite").join("plugins"))
    }

    /// Charge la configuration : défauts, puis fichier, puis variables d'environnement
    /// (`PAULWRITE_HIGHLIGHT_MAX_BYTES`). **Bloquant** : à appeler depuis un worker.
    #[must_use]
    pub fn load() -> Self {
        let mut c = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default();
        if let Some(n) = std::env::var("PAULWRITE_HIGHLIGHT_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            c.highlight_max_bytes = n;
        }
        c
    }

    /// Commande du serveur de langage de `language` (identifiant LSP : `rust`, `python`…) : celle
    /// de la configuration, sinon le serveur par défaut.
    #[must_use]
    pub fn lsp_server(&self, language: &str) -> Option<String> {
        self.lsp_servers.get(language).cloned().or_else(|| {
            DEFAULT_SERVERS
                .iter()
                .find(|(l, _)| *l == language)
                .map(|(_, c)| (*c).to_owned())
        })
    }

    /// La coloration est-elle active pour un document de `len_bytes` octets ?
    #[must_use]
    pub fn highlights(&self, len_bytes: usize) -> bool {
        self.highlight && len_bytes <= self.highlight_max_bytes
    }

    /// Un document de `len_bytes` octets peut-il être affiché avec retour à la ligne ?
    #[must_use]
    pub fn can_wrap(&self, len_bytes: usize) -> bool {
        len_bytes <= self.word_wrap_max_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valeurs_par_defaut() {
        let c = Config::default();
        assert!(c.highlight);
        assert_eq!(c.highlight_max_bytes, DEFAULT_HIGHLIGHT_MAX_BYTES);
        assert!(c.highlights(1000));
        assert!(!c.highlights(DEFAULT_HIGHLIGHT_MAX_BYTES + 1));
    }

    #[test]
    fn analyse_du_fichier() {
        let c = Config::parse(
            "# réglages\nhighlight_max_bytes = 1_000_000 # un méga\nhighlight = false\ninconnu = 3\n",
        );
        assert_eq!(c.highlight_max_bytes, 1_000_000);
        assert!(!c.highlight);
        assert!(!c.highlights(10));
    }

    #[test]
    fn serveurs_de_langage() {
        let c = Config::default();
        assert!(
            !c.lsp,
            "désactivés par défaut : ils lancent des programmes externes"
        );
        assert_eq!(c.lsp_server("rust").as_deref(), Some("rust-analyzer"));
        assert_eq!(c.lsp_server("cobol"), None);
        let c = Config::parse(
            "lsp = true\nlsp_rust = \"C:\\outils\\ra.exe --foo\" # perso\nlsp_python = pyright-langserver --stdio\n",
        );
        assert!(c.lsp);
        assert_eq!(
            c.lsp_server("rust").as_deref(),
            Some("C:\\outils\\ra.exe --foo")
        );
        assert_eq!(
            c.lsp_server("python").as_deref(),
            Some("pyright-langserver --stdio")
        );
        assert_eq!(
            c.lsp_server("json").as_deref(),
            Some("vscode-json-language-server --stdio")
        );
    }

    #[test]
    fn lignes_invalides_ignorees() {
        let c =
            Config::parse("highlight_max_bytes = beaucoup\n=\nsans egal\nhighlight = \"oui\"\n");
        assert_eq!(c, Config::default());
        // valeur entre guillemets acceptée
        assert_eq!(
            Config::parse("highlight_max_bytes = \"2048\"").highlight_max_bytes,
            2048
        );
    }
}
