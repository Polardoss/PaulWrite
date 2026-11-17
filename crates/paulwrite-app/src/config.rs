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

/// Réglages de l'application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Au-delà de cette taille, pas de coloration syntaxique (`highlight_max_bytes`).
    pub highlight_max_bytes: usize,
    /// Colorer ou non (`highlight = false` la désactive partout).
    pub highlight: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            highlight_max_bytes: DEFAULT_HIGHLIGHT_MAX_BYTES,
            highlight: true,
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

    /// La coloration est-elle active pour un document de `len_bytes` octets ?
    #[must_use]
    pub fn highlights(&self, len_bytes: usize) -> bool {
        self.highlight && len_bytes <= self.highlight_max_bytes
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
