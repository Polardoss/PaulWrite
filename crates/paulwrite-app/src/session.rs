//! Reprise de session : les onglets ouverts, la position du curseur et de la vue, le dossier et la
//! barre latérale sont enregistrés à la fermeture et rétablis au lancement suivant.
//!
//! Fichier texte `session` dans `%APPDATA%\PaulWrite`, une information par ligne :
//!
//! ```text
//! root=C:\projets\site
//! sidebar=1,250
//! active=1
//! tab=C:\projets\site\index.php|12|5|3
//! ```
//!
//! Un onglet : chemin, ligne et colonne du curseur (à partir de 1), première ligne visible (à
//! partir de 0). Le `|` est interdit dans un nom de fichier Windows : il sépare sans ambiguïté.
//! Le texte non enregistré n'est pas conservé : seuls les fichiers le sont.
//!
//! Lecture et écriture se font **sur le worker** (jamais sur le thread UI).

use std::path::{Path, PathBuf};

/// Un onglet à rouvrir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabState {
    pub path: PathBuf,
    /// Ligne du curseur, à partir de 1.
    pub line: usize,
    /// Colonne du curseur, à partir de 1.
    pub col: usize,
    /// Première ligne visible, à partir de 0.
    pub first: usize,
}

/// L'état à rétablir.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    /// Dossier du projet (barre latérale).
    pub root: Option<PathBuf>,
    /// La barre latérale est affichée.
    pub sidebar_open: bool,
    /// Largeur de la barre latérale en pixels logiques (`None` : celle par défaut).
    pub sidebar_width: Option<f32>,
    /// Indice, dans `tabs`, de l'onglet actif.
    pub active: usize,
    pub tabs: Vec<TabState>,
}

impl Session {
    /// Emplacement du fichier de session.
    #[must_use]
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("PaulWrite").join("session"))
    }

    /// Rien à rétablir.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.tabs.is_empty()
    }

    /// Analyse le contenu du fichier. Les lignes inconnues ou invalides sont ignorées : une session
    /// abîmée ne doit jamais empêcher l'éditeur de démarrer.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "root" if !value.is_empty() => s.root = Some(PathBuf::from(value)),
                "sidebar" => {
                    let (open, width) = value.split_once(',').unwrap_or((value, ""));
                    s.sidebar_open = open == "1";
                    s.sidebar_width = width.parse::<f32>().ok().filter(|w| w.is_finite());
                }
                "active" => s.active = value.parse().unwrap_or(0),
                "tab" => {
                    // Le chemin d'abord, puis trois nombres : on coupe par la droite.
                    let mut parts = value.rsplitn(4, '|');
                    let (first, col, line_no, path) =
                        (parts.next(), parts.next(), parts.next(), parts.next());
                    if let (Some(first), Some(col), Some(line_no), Some(path)) =
                        (first, col, line_no, path)
                    {
                        if let (Ok(first), Ok(col), Ok(line_no)) =
                            (first.parse(), col.parse(), line_no.parse())
                        {
                            if !path.is_empty() {
                                s.tabs.push(TabState {
                                    path: PathBuf::from(path),
                                    line: line_no,
                                    col,
                                    first,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if s.active >= s.tabs.len() {
            s.active = 0;
        }
        s
    }

    /// Texte du fichier de session.
    #[must_use]
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        if let Some(root) = &self.root {
            out.push_str(&format!("root={}\n", root.display()));
        }
        match self.sidebar_width {
            Some(w) => out.push_str(&format!(
                "sidebar={},{}\n",
                u8::from(self.sidebar_open),
                w.round()
            )),
            None => out.push_str(&format!("sidebar={}\n", u8::from(self.sidebar_open))),
        }
        out.push_str(&format!("active={}\n", self.active));
        for t in &self.tabs {
            out.push_str(&format!(
                "tab={}|{}|{}|{}\n",
                t.path.display(),
                t.line,
                t.col,
                t.first
            ));
        }
        out
    }

    /// Lit la session enregistrée et en retire ce qui n'existe plus (fichiers supprimés, dossier
    /// absent). **Bloquant** : à appeler depuis un worker.
    #[must_use]
    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(Self::path()?).ok()?;
        let mut s = Self::parse(&text);
        s.retain_existing(|p| p.is_file(), |p| p.is_dir());
        (!s.is_empty()).then_some(s)
    }

    /// Retire les onglets dont le fichier a disparu (et le dossier s'il a disparu) ; l'onglet
    /// actif reste le même quand il existe encore.
    pub fn retain_existing(
        &mut self,
        is_file: impl Fn(&Path) -> bool,
        is_dir: impl Fn(&Path) -> bool,
    ) {
        let active_path = self.tabs.get(self.active).map(|t| t.path.clone());
        self.tabs.retain(|t| is_file(&t.path));
        self.active = active_path
            .and_then(|p| self.tabs.iter().position(|t| t.path == p))
            .unwrap_or(0);
        if self.root.as_deref().is_some_and(|r| !is_dir(r)) {
            self.root = None;
            self.sidebar_open = false;
        }
    }

    /// Écrit le fichier de session (atomique : fichier temporaire puis renommage). **Bloquant**.
    ///
    /// # Errors
    /// Dossier ou fichier non inscriptible.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.serialize())?;
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            root: Some(PathBuf::from(r"C:\projets\site")),
            sidebar_open: true,
            sidebar_width: Some(310.0),
            active: 1,
            tabs: vec![
                TabState {
                    path: PathBuf::from(r"C:\projets\site\index.php"),
                    line: 12,
                    col: 5,
                    first: 3,
                },
                TabState {
                    path: PathBuf::from(r"C:\projets\site\mon fichier (1).css"),
                    line: 1,
                    col: 1,
                    first: 0,
                },
            ],
        }
    }

    #[test]
    fn aller_retour() {
        let s = sample();
        assert_eq!(Session::parse(&s.serialize()), s);
        // sans largeur ni dossier
        let s = Session {
            tabs: vec![TabState {
                path: PathBuf::from("a.txt"),
                line: 2,
                col: 3,
                first: 1,
            }],
            ..Session::default()
        };
        assert_eq!(Session::parse(&s.serialize()), s);
    }

    #[test]
    fn un_fichier_abime_ne_casse_rien() {
        let s = Session::parse("n'importe quoi\ntab=sans|nombres\ntab=|1|1|1\ntab=a.rs|x|1|1\nactive=zz\nsidebar=1,abc\n=\n");
        assert!(s.tabs.is_empty());
        assert_eq!(s.active, 0);
        assert!(s.sidebar_open);
        assert_eq!(s.sidebar_width, None);
        assert!(Session::parse("").is_empty());
        // un indice actif hors limites revient au premier onglet
        let s = Session::parse("tab=a.rs|1|1|0\nactive=7\n");
        assert_eq!(s.active, 0);
        assert_eq!(s.tabs.len(), 1);
    }

    #[test]
    fn les_fichiers_disparus_sont_retires() {
        let mut s = sample();
        s.active = 1;
        s.retain_existing(|p| p.ends_with("mon fichier (1).css"), |_| true);
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.active, 0, "l'onglet actif a suivi");
        // l'onglet actif disparaît : on revient au premier
        let mut s = sample();
        s.active = 1;
        s.retain_existing(|p| p.ends_with("index.php"), |_| true);
        assert_eq!((s.tabs.len(), s.active), (1, 0));
        // dossier disparu : plus de barre latérale
        let mut s = sample();
        s.retain_existing(|_| true, |_| false);
        assert_eq!(s.root, None);
        assert!(!s.sidebar_open);
        assert_eq!(s.tabs.len(), 2);
    }
}
