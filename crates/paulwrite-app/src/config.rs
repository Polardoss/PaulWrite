//! Configuration utilisateur : fichier `config.toml` dans `%APPDATA%\PaulWrite`.
//!
//! Format : sous-ensemble minimal de TOML (`clé = valeur`, commentaires `#`, entiers, booléens et
//! chaînes). Un analyseur TOML complet (crate `toml`) sera ajouté avec les thèmes (étape 6) ;
//! d'ici là, une dépendance de plus pour trois réglages n'est pas justifiée (règle n°7).
//!
//! Le fichier est lu **sur le worker**, jamais sur le thread UI.

use std::path::PathBuf;

use crate::i18n::Lang;

/// Taille maximale (octets) d'un fichier coloré par défaut. Au-delà, la coloration est désactivée
/// (voir `docs/benchmarks.md` pour les mesures qui fondent cette valeur).
pub const DEFAULT_HIGHLIGHT_MAX_BYTES: usize = 1024 * 1024;

/// Taille maximale (octets) d'un fichier affiché avec retour à la ligne par défaut : au-delà, les
/// lignes ne sont pas coupées (voir `docs/benchmarks.md`).
pub const DEFAULT_WRAP_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Taille de police par défaut, minimale et maximale (pixels logiques).
pub const DEFAULT_FONT_SIZE: u32 = 15;
pub const MIN_FONT_SIZE: u32 = 8;
pub const MAX_FONT_SIZE: u32 = 48;

/// Réglages de l'application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Langue de l'interface : `language = "en"` ou `"fr"` ; absente ou `"auto"` : celle de Windows.
    pub language: Option<Lang>,
    /// Taille de la police du texte, en pixels (`font_size`, 8 à 48 ; Ctrl+molette l'ajuste).
    pub font_size: u32,
    /// Largeur d'une tabulation, en colonnes (`tab_size`, 1 à 16).
    pub tab_size: usize,
    /// Rouvrir les onglets, le dossier et la position du curseur au lancement
    /// (`restore_session = false` pour toujours démarrer vide).
    pub restore_session: bool,
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
const DEFAULT_SERVERS: [(&str, &str); 8] = [
    ("rust", "rust-analyzer"),
    ("python", "pylsp"),
    ("javascript", "typescript-language-server --stdio"),
    ("json", "vscode-json-language-server --stdio"),
    ("html", "vscode-html-language-server --stdio"),
    ("css", "vscode-css-language-server --stdio"),
    ("toml", "taplo lsp stdio"),
    ("php", "intelephense --stdio"),
];

impl Default for Config {
    fn default() -> Self {
        Self {
            language: None,
            font_size: DEFAULT_FONT_SIZE,
            tab_size: crate::editing::DEFAULT_TAB_WIDTH,
            restore_session: true,
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
                "language" => c.language = Lang::from_code(value),
                "font_size" => {
                    if let Ok(n) = value.parse::<u32>() {
                        c.font_size = n.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                    }
                }
                "tab_size" => {
                    if let Ok(n) = value.parse::<usize>() {
                        c.tab_size = n.clamp(1, 16);
                    }
                }
                "restore_session" => {
                    if let Ok(b) = value.parse::<bool>() {
                        c.restore_session = b;
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

    /// Langue de l'interface : celle du réglage, sinon celle de Windows.
    #[must_use]
    pub fn lang(&self) -> Lang {
        self.language.unwrap_or_else(Lang::system)
    }

    /// Texte de `config.toml` dans lequel la clé `key` vaut `value` : la ligne existante est
    /// remplacée (commentaire de fin et fin de ligne conservés), sinon la ligne est ajoutée à la
    /// fin. Le reste du fichier (commentaires, autres clés) est inchangé.
    #[must_use]
    pub fn with_setting(text: &str, key: &str, value: &str) -> String {
        let new_line = format!("{key} = \"{value}\"");
        let mut out = String::with_capacity(text.len() + new_line.len() + 1);
        let mut done = false;
        for line in text.split_inclusive('\n') {
            let code = line.split('#').next().unwrap_or("");
            let is_key = code.split_once('=').is_some_and(|(k, _)| k.trim() == key);
            if is_key && !done {
                done = true;
                let eol = &line[line.trim_end_matches(['\r', '\n']).len()..];
                let comment = line.find('#').map(|i| format!(" {}", line[i..].trim_end()));
                out.push_str(&new_line);
                out.push_str(&comment.unwrap_or_default());
                out.push_str(if eol.is_empty() { "\n" } else { eol });
            } else {
                out.push_str(line);
            }
        }
        if !done {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&new_line);
            out.push('\n');
        }
        out
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
    fn police_et_tabulation() {
        let c = Config::parse(
            "font_size = 20
tab_size = 8
",
        );
        assert_eq!((c.font_size, c.tab_size), (20, 8));
        // valeurs hors bornes ramenées dedans, valeurs invalides ignorées
        let c = Config::parse(
            "font_size = 500
tab_size = 0
",
        );
        assert_eq!((c.font_size, c.tab_size), (MAX_FONT_SIZE, 1));
        let c = Config::parse(
            "font_size = 2
tab_size = 99
",
        );
        assert_eq!((c.font_size, c.tab_size), (MIN_FONT_SIZE, 16));
        let c = Config::parse(
            "font_size = grand
tab_size = -3
",
        );
        assert_eq!(
            (c.font_size, c.tab_size),
            (DEFAULT_FONT_SIZE, crate::editing::DEFAULT_TAB_WIDTH)
        );
        // le réglage enregistré par le zoom est relu tel quel
        let text = Config::with_setting(
            "# rien
",
            "font_size",
            "18",
        );
        assert_eq!(Config::parse(&text).font_size, 18);
    }

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

    #[test]
    fn la_langue_vient_du_fichier() {
        assert_eq!(Config::parse("language = \"fr\"").language, Some(Lang::Fr));
        assert_eq!(Config::parse("language = en").language, Some(Lang::En));
        assert_eq!(Config::parse("language = \"auto\"").language, None);
        assert_eq!(Config::parse("language = \"klingon\"").language, None);
        assert_eq!(Config::parse("").language, None);
        let c = Config {
            language: Some(Lang::Fr),
            ..Config::default()
        };
        assert_eq!(c.lang(), Lang::Fr);
    }

    #[test]
    fn ecrire_un_reglage_sans_toucher_au_reste() {
        let w = Config::with_setting;
        // absent : ajouté à la fin
        assert_eq!(w("", "language", "fr"), "language = \"fr\"\n");
        assert_eq!(
            w("highlight = true", "language", "fr"),
            "highlight = true\nlanguage = \"fr\"\n"
        );
        // présent : remplacé sur place, commentaire et fins de ligne conservés
        assert_eq!(
            w(
                "# réglages\nlanguage = \"en\" # langue\nlsp = true\n",
                "language",
                "fr"
            ),
            "# réglages\nlanguage = \"fr\" # langue\nlsp = true\n"
        );
        assert_eq!(
            w("a = 1\r\nlanguage = en\r\nb = 2\r\n", "language", "fr"),
            "a = 1\r\nlanguage = \"fr\"\r\nb = 2\r\n"
        );
        // une clé qui contient le nom, ou un commentaire, n'est pas la clé
        assert_eq!(
            w("# language = en\nlanguage_x = 1\n", "language", "fr"),
            "# language = en\nlanguage_x = 1\nlanguage = \"fr\"\n"
        );
        // le résultat est relu correctement, y compris deux fois de suite
        let once = w("lsp = true\n", "language", "fr");
        let twice = w(&once, "language", "en");
        assert_eq!(Config::parse(&twice).language, Some(Lang::En));
        assert!(Config::parse(&twice).lsp);
        assert_eq!(twice.matches("language").count(), 1);
    }
}
