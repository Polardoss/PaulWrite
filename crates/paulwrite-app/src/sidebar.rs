//! Barre latérale : arborescence du dossier ouvert. Données et logique pures (testables sans
//! fenêtre) ; les listings de dossiers viennent du worker (le thread UI ne lit jamais le disque).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::i18n::Lang;

use paulwrite_render::{Sidebar, SidebarKind, SidebarRow};

/// Un élément d'un dossier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// Noms de dossiers jamais affichés (gestionnaires de versions).
#[must_use]
pub fn is_hidden_dir(name: &str) -> bool {
    matches!(name, ".git" | ".svn" | ".hg" | "CVS")
}

/// Trie : dossiers d'abord, puis par nom sans tenir compte de la casse.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Ce que produit un clic sur une ligne.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    /// Ouvrir ce fichier.
    OpenFile(PathBuf),
    /// Un dossier a été replié ou déplié.
    Toggled,
    /// Rien (titre, hors des lignes).
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Header,
    Item {
        path: PathBuf,
        name: String,
        depth: usize,
        is_dir: bool,
        open: bool,
    },
}

/// Arborescence affichée dans la barre latérale.
#[derive(Debug)]
pub struct SidebarTree {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    listed: HashMap<PathBuf, Vec<Entry>>,
    /// Dossiers dont le listing a été demandé au worker et n'est pas encore revenu.
    pending: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    /// Première ligne affichée.
    scroll: usize,
    /// Lignes à plat (titre, puis l'arbre déplié) : reconstruites quand l'arbre change.
    flat: Vec<Node>,
    flat_dirty: bool,
    /// Contenu transmis au moteur de rendu, reconstruit quand il change.
    cache: Option<(usize, usize, Sidebar)>,
    /// Langue du titre de la colonne.
    lang: Lang,
}

impl SidebarTree {
    /// Arborescence d'un dossier : la racine est dépliée (son listing est à demander).
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        Self {
            root,
            expanded,
            listed: HashMap::new(),
            pending: HashSet::new(),
            selected: None,
            scroll: 0,
            flat: Vec::new(),
            flat_dirty: true,
            cache: None,
            lang: Lang::En,
        }
    }

    /// Change la langue du titre (« Folders » / « Dossiers »).
    pub fn set_lang(&mut self, lang: Lang) {
        if self.lang != lang {
            self.lang = lang;
            self.cache = None;
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Dossiers dépliés dont le contenu n'est pas connu : à lister (demandés une seule fois).
    pub fn take_listing_requests(&mut self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|p| !self.listed.contains_key(*p) && !self.pending.contains(*p))
            .cloned()
            .collect();
        v.sort();
        self.pending.extend(v.iter().cloned());
        v
    }

    /// Contenu d'un dossier, reçu du worker.
    pub fn set_listing(&mut self, dir: PathBuf, entries: Vec<Entry>) {
        self.pending.remove(&dir);
        self.listed.insert(dir, entries);
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.flat_dirty = true;
        self.cache = None;
    }

    fn rebuild(&mut self) {
        if !self.flat_dirty {
            return;
        }
        self.flat_dirty = false;
        self.flat.clear();
        self.flat.push(Node::Header);
        let name = self
            .root
            .file_name()
            .map_or_else(|| self.root.to_string_lossy(), |n| n.to_string_lossy())
            .into_owned();
        let open = self.expanded.contains(&self.root);
        self.flat.push(Node::Item {
            path: self.root.clone(),
            name,
            depth: 0,
            is_dir: true,
            open,
        });
        if open {
            let root = self.root.clone();
            self.push_children(&root, 1);
        }
    }

    fn push_children(&mut self, dir: &Path, depth: usize) {
        let Some(entries) = self.listed.get(dir).cloned() else {
            return;
        };
        for e in entries {
            let path = dir.join(&e.name);
            let open = e.is_dir && self.expanded.contains(&path);
            self.flat.push(Node::Item {
                path: path.clone(),
                name: e.name,
                depth,
                is_dir: e.is_dir,
                open,
            });
            if open {
                self.push_children(&path, depth + 1);
            }
        }
    }

    /// Nombre de lignes de l'arbre à plat.
    #[must_use]
    pub fn len(&mut self) -> usize {
        self.rebuild();
        self.flat.len()
    }

    #[must_use]
    pub fn is_empty(&mut self) -> bool {
        self.len() == 0
    }

    /// Fait défiler de `lines` lignes ; `capacity` : lignes affichables.
    pub fn scroll_by(&mut self, lines: isize, capacity: usize) {
        self.rebuild();
        let max = self.flat.len().saturating_sub(capacity.max(1));
        let target = (self.scroll as isize + lines).clamp(0, max as isize) as usize;
        if target != self.scroll {
            self.scroll = target;
            self.cache = None;
        }
    }

    /// Clic sur la ligne `row` de la partie affichée (à partir de `scroll`).
    pub fn click(&mut self, row: usize) -> Click {
        self.rebuild();
        let Some(Node::Item {
            path, is_dir, open, ..
        }) = self.flat.get(self.scroll + row).cloned()
        else {
            return Click::Nothing;
        };
        self.selected = Some(path.clone());
        self.cache = None;
        if is_dir {
            if open {
                self.expanded.remove(&path);
            } else {
                self.expanded.insert(path);
            }
            self.invalidate();
            Click::Toggled
        } else {
            Click::OpenFile(path)
        }
    }

    /// Lignes à afficher pour une fenêtre de `capacity` lignes.
    pub fn render(&mut self, capacity: usize) -> &Sidebar {
        self.rebuild();
        let stale = !matches!(&self.cache, Some((cap, scroll, _)) if *cap == capacity && *scroll == self.scroll);
        if stale {
            let rows = self
                .flat
                .iter()
                .skip(self.scroll)
                .take(capacity)
                .map(|n| match n {
                    Node::Header => SidebarRow {
                        label: self.lang.pick("Folders", "Dossiers").to_owned(),
                        depth: 0,
                        kind: SidebarKind::Header,
                        selected: false,
                    },
                    Node::Item {
                        path,
                        name,
                        depth,
                        is_dir,
                        open,
                    } => SidebarRow {
                        label: name.clone(),
                        depth: *depth,
                        kind: match (is_dir, open) {
                            (false, _) => SidebarKind::File,
                            (true, false) => SidebarKind::FolderClosed,
                            (true, true) => SidebarKind::FolderOpen,
                        },
                        selected: self.selected.as_ref() == Some(path),
                    },
                })
                .collect();
            self.cache = Some((capacity, self.scroll, Sidebar { rows }));
        }
        &self.cache.as_ref().expect("cache rempli").2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.into(),
            is_dir,
        }
    }

    fn labels(t: &mut SidebarTree, cap: usize) -> Vec<String> {
        t.render(cap)
            .rows
            .iter()
            .map(|r| format!("{}{}", " ".repeat(r.depth), r.label))
            .collect()
    }

    #[test]
    fn tri_dossiers_d_abord_sans_casse() {
        let mut v = vec![
            e("b.rs", false),
            e("Zed", true),
            e("a.rs", false),
            e("alpha", true),
        ];
        sort_entries(&mut v);
        let names: Vec<_> = v.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zed", "a.rs", "b.rs"]);
        assert!(is_hidden_dir(".git") && !is_hidden_dir(".github"));
    }

    #[test]
    fn la_racine_est_listee_une_seule_fois_puis_l_arbre_s_affiche() {
        let root = PathBuf::from("proj");
        let mut t = SidebarTree::new(root.clone());
        assert_eq!(t.take_listing_requests(), vec![root.clone()]);
        assert!(t.take_listing_requests().is_empty(), "déjà demandé");
        // avant la réponse : le titre et la racine seulement
        assert_eq!(labels(&mut t, 10), ["Folders", "proj"]);
        t.set_listing(root.clone(), vec![e("src", true), e("README.md", false)]);
        assert_eq!(
            labels(&mut t, 10),
            ["Folders", "proj", " src", " README.md"]
        );
        // déplier src : son listing est demandé, puis affiché
        assert_eq!(t.click(2), Click::Toggled);
        assert_eq!(t.take_listing_requests(), vec![root.join("src")]);
        t.set_listing(root.join("src"), vec![e("main.rs", false)]);
        assert_eq!(
            labels(&mut t, 10),
            ["Folders", "proj", " src", "  main.rs", " README.md"]
        );
        // replier : les enfants disparaissent ; le contenu reste en mémoire (pas de relecture)
        assert_eq!(t.click(2), Click::Toggled);
        assert_eq!(
            labels(&mut t, 10),
            ["Folders", "proj", " src", " README.md"]
        );
        assert!(t.take_listing_requests().is_empty());
        assert_eq!(t.click(2), Click::Toggled);
        assert!(t.take_listing_requests().is_empty(), "déjà listé");
    }

    #[test]
    fn un_fichier_s_ouvre_et_se_selectionne() {
        let root = PathBuf::from("proj");
        let mut t = SidebarTree::new(root.clone());
        t.take_listing_requests();
        t.set_listing(root.clone(), vec![e("a.txt", false)]);
        assert_eq!(t.click(0), Click::Nothing, "le titre");
        assert_eq!(t.click(2), Click::OpenFile(root.join("a.txt")));
        assert!(t.render(10).rows[2].selected);
        assert_eq!(t.click(9), Click::Nothing, "hors des lignes");
    }

    #[test]
    fn defilement_borne_et_fenetre_affichee() {
        let root = PathBuf::from("p");
        let mut t = SidebarTree::new(root.clone());
        t.take_listing_requests();
        t.set_listing(
            root,
            (0..30).map(|i| e(&format!("f{i:02}"), false)).collect(),
        );
        assert_eq!(t.len(), 32);
        t.scroll_by(5, 10);
        assert_eq!(labels(&mut t, 10)[0], " f03");
        t.scroll_by(1000, 10);
        assert_eq!(labels(&mut t, 10).len(), 10);
        assert_eq!(labels(&mut t, 10)[9], " f29");
        t.scroll_by(-1000, 10);
        assert_eq!(labels(&mut t, 10)[0], "Folders");
        // un clic tient compte du défilement
        t.scroll_by(4, 10);
        assert_eq!(t.click(0), Click::OpenFile(PathBuf::from("p").join("f02")));
    }

    #[test]
    fn titre_dans_la_langue_choisie() {
        let mut t = SidebarTree::new(PathBuf::from("/proj"));
        assert_eq!(labels(&mut t, 10)[0], "Folders");
        t.set_lang(Lang::Fr);
        assert_eq!(labels(&mut t, 10)[0], "Dossiers");
        t.set_lang(Lang::En);
        assert_eq!(labels(&mut t, 10)[0], "Folders");
    }
}
