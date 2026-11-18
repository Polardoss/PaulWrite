//! Barre latérale : arborescence du dossier ouvert. Données et logique pures (testables sans
//! fenêtre) ; les listings de dossiers viennent du worker (le thread UI ne lit jamais le disque).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::i18n::Lang;

use std::sync::Arc;

use paulwrite_render::{FileIcon, GitMark, Sidebar, SidebarKind, SidebarRow};

use crate::git::GitStatus;

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
    /// Le dossier ouvert lui-même (premier niveau de l'arbre).
    Root {
        open: bool,
    },
    /// Entrée numéro `index` du listing du dossier `dir` (indice dans `SidebarTree::dirs`). Ni nom
    /// ni chemin ici : un dossier de 100 000 fichiers ne coûte pas 200 000 allocations à chaque
    /// dépli ; on les retrouve à la demande, pour les quelques lignes affichées.
    Item {
        dir: u32,
        index: u32,
        depth: u32,
        is_dir: bool,
        open: bool,
    },
}

/// Famille d'un fichier d'après son extension (icône de la barre latérale).
#[must_use]
pub fn icon_for(name: &str) -> FileIcon {
    let ext = name
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => FileIcon::Rust,
        "py" | "pyw" | "pyi" => FileIcon::Python,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => FileIcon::Script,
        "html" | "htm" | "css" | "scss" => FileIcon::Web,
        "json" | "toml" | "yaml" | "yml" | "xml" | "ini" | "lock" => FileIcon::Data,
        "md" | "markdown" | "txt" | "rst" => FileIcon::Doc,
        _ => FileIcon::Plain,
    }
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
    /// Dossiers dont les entrées figurent dans `flat` (cible de `Node::Item::dir`).
    dirs: Vec<PathBuf>,
    flat_dirty: bool,
    /// Contenu transmis au moteur de rendu, reconstruit quand il change.
    cache: Option<(usize, usize, Sidebar)>,
    /// Langue du titre de la colonne.
    lang: Lang,
    /// État Git du dépôt (barre colorée, lettres, points).
    git: Option<Arc<GitStatus>>,
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
            dirs: Vec::new(),
            flat_dirty: true,
            cache: None,
            lang: Lang::En,
            git: None,
        }
    }

    /// Nouvel état Git (`None` : pas de dépôt) ; les lignes sont redessinées.
    pub fn set_git(&mut self, git: Option<Arc<GitStatus>>) {
        if self.git != git {
            self.git = git;
            self.cache = None;
        }
    }

    /// La marque Git d'un fichier ou d'un dossier.
    fn git_mark(&self, path: &Path, is_dir: bool) -> GitMark {
        self.git
            .as_deref()
            .map_or(GitMark::None, |git| git.mark_of(path, is_dir))
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

    /// Le contenu de ce dossier est connu (affiché ou déjà affiché) : un changement sur le disque
    /// demande de le relire.
    #[must_use]
    pub fn is_listed(&self, dir: &Path) -> bool {
        self.listed.contains_key(dir)
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
        self.dirs.clear();
        self.flat.push(Node::Header);
        let open = self.expanded.contains(&self.root);
        self.flat.push(Node::Root { open });
        if open {
            let root = self.root.clone();
            self.push_children(&root, 1);
        }
    }

    fn push_children(&mut self, dir: &Path, depth: u32) {
        // le listing est sorti de la table le temps du parcours (pas de copie) : la récursion
        // lit les listings des sous-dossiers
        let Some(entries) = self.listed.remove(dir) else {
            return;
        };
        let at = self.dirs.len() as u32;
        self.dirs.push(dir.to_path_buf());
        self.flat.reserve(entries.len());
        for (index, e) in entries.iter().enumerate() {
            // seuls les dossiers ont besoin de leur chemin (savoir s'ils sont dépliés)
            let path = e.is_dir.then(|| dir.join(&e.name));
            let open = path.as_ref().is_some_and(|p| self.expanded.contains(p));
            self.flat.push(Node::Item {
                dir: at,
                index: index as u32,
                depth,
                is_dir: e.is_dir,
                open,
            });
            if let (true, Some(path)) = (open, path) {
                self.push_children(&path, depth + 1);
            }
        }
        self.listed.insert(dir.to_path_buf(), entries);
    }

    /// Chemin de la ligne `at` de l'arbre à plat (pas de l'en-tête).
    fn path_of(&self, at: usize) -> Option<PathBuf> {
        match self.flat.get(at)? {
            Node::Header => None,
            Node::Root { .. } => Some(self.root.clone()),
            Node::Item { dir, index, .. } => {
                let dir = self.dirs.get(*dir as usize)?;
                let entry = self.listed.get(dir)?.get(*index as usize)?;
                Some(dir.join(&entry.name))
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
        let at = self.scroll + row;
        let (is_dir, open) = match self.flat.get(at) {
            Some(Node::Root { open }) => (true, *open),
            Some(Node::Item { is_dir, open, .. }) => (*is_dir, *open),
            _ => return Click::Nothing,
        };
        let Some(path) = self.path_of(at) else {
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

    /// Fait défiler jusqu'à la fraction `fraction` (0..1) de l'arbre : glisser la barre de
    /// défilement.
    pub fn scroll_to_fraction(&mut self, fraction: f32, capacity: usize) {
        self.rebuild();
        let max = self.flat.len().saturating_sub(capacity.max(1));
        let target = (fraction.clamp(0.0, 1.0) * max as f32).round() as usize;
        if target != self.scroll {
            self.scroll = target;
            self.cache = None;
        }
    }

    /// Lignes à afficher pour une fenêtre de `capacity` lignes.
    pub fn render(&mut self, capacity: usize) -> &Sidebar {
        self.rebuild();
        let stale = !matches!(&self.cache, Some((cap, scroll, _)) if *cap == capacity && *scroll == self.scroll);
        if stale {
            let end = (self.scroll + capacity).min(self.flat.len());
            let rows = (self.scroll..end)
                .map(|at| self.row_at(at))
                .collect::<Vec<_>>();
            let total = self.flat.len();
            let scroll = (total > capacity).then_some((self.scroll, capacity, total));
            self.cache = Some((capacity, self.scroll, Sidebar { rows, scroll }));
        }
        &self.cache.as_ref().expect("cache rempli").2
    }

    /// La ligne affichée pour la ligne `at` de l'arbre à plat.
    fn row_at(&self, at: usize) -> SidebarRow {
        let selected = |path: &Path| self.selected.as_deref() == Some(path);
        match &self.flat[at] {
            Node::Header => SidebarRow {
                git: GitMark::None,
                label: self.lang.pick("Folders", "Dossiers").to_owned(),
                depth: 0,
                kind: SidebarKind::Header,
                selected: false,
                icon: FileIcon::Plain,
            },
            Node::Root { open } => SidebarRow {
                git: self.git_mark(&self.root, true),
                label: self
                    .root
                    .file_name()
                    .map_or_else(|| self.root.to_string_lossy(), |n| n.to_string_lossy())
                    .into_owned(),
                depth: 0,
                kind: if *open {
                    SidebarKind::FolderOpen
                } else {
                    SidebarKind::FolderClosed
                },
                selected: selected(&self.root),
                icon: FileIcon::Plain,
            },
            Node::Item {
                dir,
                index,
                depth,
                is_dir,
                open,
            } => {
                let dir_path = &self.dirs[*dir as usize];
                let name = &self.listed[dir_path][*index as usize].name;
                SidebarRow {
                    git: self.git_mark(&dir_path.join(name), *is_dir),
                    label: name.clone(),
                    depth: *depth as usize,
                    kind: match (is_dir, open) {
                        (false, _) => SidebarKind::File,
                        (true, false) => SidebarKind::FolderClosed,
                        (true, true) => SidebarKind::FolderOpen,
                    },
                    selected: self.selected.is_some() && selected(&dir_path.join(name)),
                    icon: if *is_dir {
                        FileIcon::Plain
                    } else {
                        icon_for(name)
                    },
                }
            }
        }
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

    #[test]
    fn icones_par_extension() {
        assert_eq!(icon_for("main.rs"), FileIcon::Rust);
        assert_eq!(icon_for("App.TSX"), FileIcon::Script);
        assert_eq!(icon_for("setup.py"), FileIcon::Python);
        assert_eq!(icon_for("index.html"), FileIcon::Web);
        assert_eq!(icon_for("Cargo.toml"), FileIcon::Data);
        assert_eq!(icon_for("README.md"), FileIcon::Doc);
        assert_eq!(icon_for("LICENSE"), FileIcon::Plain);
        assert_eq!(icon_for(".gitignore"), FileIcon::Plain);
        assert_eq!(icon_for("archive.tar.gz"), FileIcon::Plain);
    }

    #[test]
    fn barre_de_defilement_et_fraction() {
        let mut t = SidebarTree::new(PathBuf::from("/proj"));
        let _ = t.take_listing_requests();
        let entries: Vec<Entry> = (0..30).map(|i| e(&format!("f{i:02}.rs"), false)).collect();
        t.set_listing(PathBuf::from("/proj"), entries);
        // 32 lignes (titre, racine, 30 fichiers) pour 10 affichées
        let sb = t.render(10);
        assert_eq!(sb.scroll, Some((0, 10, 32)));
        assert_eq!(sb.rows[2].icon, FileIcon::Rust);
        t.scroll_to_fraction(1.0, 10);
        assert_eq!(t.render(10).scroll, Some((22, 10, 32)));
        t.scroll_to_fraction(0.5, 10);
        assert_eq!(t.render(10).scroll, Some((11, 10, 32)));
        t.scroll_to_fraction(-3.0, 10);
        assert_eq!(t.render(10).scroll, Some((0, 10, 32)));
        // tout tient : pas de barre
        assert_eq!(t.render(40).scroll, None);
    }

    #[test]
    fn relire_un_dossier_fait_apparaitre_et_disparaitre_ses_fichiers() {
        let root = PathBuf::from("/proj");
        let mut t = SidebarTree::new(root.clone());
        assert!(!t.is_listed(&root));
        let _ = t.take_listing_requests();
        t.set_listing(root.clone(), vec![e("a.rs", false)]);
        assert!(t.is_listed(&root));
        assert_eq!(labels(&mut t, 10), ["Folders", "proj", " a.rs"]);
        // un fichier est créé sur le disque : le listing relu le montre, trié
        t.set_listing(root.clone(), vec![e("a.rs", false), e("b.rs", false)]);
        assert_eq!(labels(&mut t, 10), ["Folders", "proj", " a.rs", " b.rs"]);
        // puis supprimé
        t.set_listing(root.clone(), vec![e("b.rs", false)]);
        assert_eq!(labels(&mut t, 10), ["Folders", "proj", " b.rs"]);
        // un dossier jamais lu n'est pas relu
        assert!(!t.is_listed(&root.join("src")));
    }
}
