//! Langue de l'interface : anglais (comme Sublime Text) ou français. Les textes sont écrits en
//! deux versions à l'endroit où ils servent (`lang.pick(en, fr)`) : pas de fichier de traduction à
//! charger, donc rien à lire au démarrage.

use paulwrite_render::platform;

/// Langue de l'interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Fr,
}

impl Lang {
    /// Le texte dans la langue courante.
    #[must_use]
    pub fn pick<T>(self, en: T, fr: T) -> T {
        match self {
            Lang::En => en,
            Lang::Fr => fr,
        }
    }

    /// Code utilisé dans `config.toml` (`language = "fr"`).
    #[must_use]
    pub fn code(self) -> &'static str {
        self.pick("en", "fr")
    }

    /// Nom de la langue, dans cette langue.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.pick("English", "Français")
    }

    /// Langue désignée par un code (`en`, `fr`, insensible à la casse) ; `None` pour « auto » ou
    /// une valeur inconnue.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Lang::En),
            "fr" | "french" | "français" | "francais" => Some(Lang::Fr),
            _ => None,
        }
    }

    /// Langue d'affichage de Windows : le français si c'est la langue du système, sinon l'anglais.
    #[must_use]
    pub fn system() -> Self {
        if platform::ui_language_is_french() {
            Lang::Fr
        } else {
            Lang::En
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choix_du_texte_et_codes() {
        assert_eq!(Lang::En.pick("yes", "oui"), "yes");
        assert_eq!(Lang::Fr.pick("yes", "oui"), "oui");
        assert_eq!(Lang::from_code(" FR "), Some(Lang::Fr));
        assert_eq!(Lang::from_code("English"), Some(Lang::En));
        assert_eq!(Lang::from_code("auto"), None);
        assert_eq!(Lang::from_code(""), None);
        for l in [Lang::En, Lang::Fr] {
            assert_eq!(Lang::from_code(l.code()), Some(l));
        }
        assert_eq!(Lang::Fr.name(), "Français");
        // la détection du système ne plante pas et renvoie l'une des deux langues
        let _ = Lang::system();
    }
}
