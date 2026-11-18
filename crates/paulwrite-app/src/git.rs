//! État Git du dossier ouvert : branche, avance / retard sur le serveur, état de chaque fichier
//! (modifié, ajouté, supprimé, non suivi, en conflit, ignoré). **Lecture seule.**
//!
//! On interroge le programme `git` lui-même (`git status --porcelain=v2`) : aucune dépendance
//! ajoutée, résultat identique à celui des autres outils Git, `.gitignore` et sous-modules inclus.
//! Tout se passe sur un **thread dédié** ([`spawn`]) : les demandes qui s'accumulent sont fusionnées
//! (seule la dernière compte), deux exécutions sont espacées d'au moins [`MIN_SPACING`], et une
//! exécution trop longue est abandonnée ([`TIMEOUT`]). `--no-optional-locks` : on ne crée jamais de
//! verrou qui gênerait un autre outil Git en cours d'utilisation.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use paulwrite_render::GitMark;

/// Durée maximale d'un `git status` (un très gros dépôt peut être lent : on garde alors l'ancien
/// état plutôt que d'attendre).
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Délai minimal entre deux exécutions (une tempête d'événements disque ne lance pas Git en boucle).
pub const MIN_SPACING: Duration = Duration::from_millis(300);

/// État d'un fichier (ou d'un dossier entier non suivi / ignoré).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
    Ignored,
}

/// Ce que dit `git status`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// Racine du dépôt (le dossier qui contient `.git`).
    pub root: PathBuf,
    /// Branche courante, ou `(abc1234)` en tête détachée.
    pub branch: String,
    /// Commits à envoyer / à récupérer (0 sans branche suivie).
    pub ahead: u32,
    pub behind: u32,
    /// Nombre de fichiers modifiés, ajoutés, supprimés, renommés, non suivis ou en conflit.
    pub changes: usize,
    /// État des fichiers, par chemin absolu. Un dossier non suivi ou ignoré y figure lui-même (ses
    /// fichiers en héritent : voir [`state_of`](Self::state_of)).
    files: HashMap<PathBuf, FileState>,
    /// Dossiers (chemins absolus, racine comprise) qui contiennent au moins un changement.
    dirty_dirs: HashSet<PathBuf>,
}

impl GitStatus {
    /// État d'un fichier ou d'un dossier : le sien, sinon celui du dossier non suivi ou ignoré qui
    /// le contient.
    #[must_use]
    pub fn state_of(&self, path: &Path) -> Option<FileState> {
        if let Some(s) = self.files.get(path) {
            return Some(*s);
        }
        for dir in path.ancestors().skip(1) {
            if dir == self.root {
                break;
            }
            if let Some(s @ (FileState::Untracked | FileState::Ignored)) = self.files.get(dir) {
                return Some(*s);
            }
        }
        None
    }

    /// La marque affichée pour un fichier ou un dossier (lettre, couleur, point).
    #[must_use]
    pub fn mark_of(&self, path: &Path, is_dir: bool) -> GitMark {
        match self.state_of(path) {
            Some(FileState::Modified) => GitMark::Modified,
            Some(FileState::Added) => GitMark::Added,
            Some(FileState::Deleted) => GitMark::Deleted,
            Some(FileState::Renamed) => GitMark::Renamed,
            Some(FileState::Untracked) => GitMark::Untracked,
            Some(FileState::Conflict) => GitMark::Conflict,
            Some(FileState::Ignored) => GitMark::Ignored,
            None if is_dir && self.dir_dirty(path) => GitMark::Dirty,
            None => GitMark::None,
        }
    }

    /// Le dossier contient au moins un changement (hors fichiers ignorés).
    #[must_use]
    pub fn dir_dirty(&self, dir: &Path) -> bool {
        self.dirty_dirs.contains(dir)
    }
}

/// Un chemin donné par Git (relatif à la racine, séparateur `/`) en chemin absolu.
fn absolute(root: &Path, rel: &str) -> PathBuf {
    rel.trim_end_matches('/')
        .split('/')
        .filter(|c| !c.is_empty())
        .fold(root.to_path_buf(), |p, c| p.join(c))
}

/// État d'un enregistrement ordinaire (`1`) ou de renommage (`2`) d'après son code `XY` (X : index,
/// Y : arbre de travail).
fn state_from_xy(xy: &[u8], rename: bool) -> FileState {
    let (x, y) = (
        xy.first().copied().unwrap_or(b'.'),
        xy.get(1).copied().unwrap_or(b'.'),
    );
    if rename {
        FileState::Renamed
    } else if x == b'A' {
        FileState::Added
    } else if x == b'D' || y == b'D' {
        FileState::Deleted
    } else {
        FileState::Modified
    }
}

/// Analyse la sortie de `git status --porcelain=v2 --branch -z --ignored` (enregistrements séparés
/// par des octets nuls).
#[must_use]
pub fn parse_status(root: &Path, output: &str) -> GitStatus {
    let mut st = GitStatus {
        root: root.to_path_buf(),
        ..GitStatus::default()
    };
    let mut oid = String::new();
    let mut head = String::new();
    let mut records = output.split('\0');
    while let Some(rec) = records.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            if let Some(v) = h.strip_prefix("branch.oid ") {
                oid = v.to_owned();
            } else if let Some(v) = h.strip_prefix("branch.head ") {
                head = v.to_owned();
            } else if let Some(v) = h.strip_prefix("branch.ab ") {
                let mut it = v.split(' ');
                st.ahead = it
                    .next()
                    .and_then(|a| a.strip_prefix('+'))
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(0);
                st.behind = it
                    .next()
                    .and_then(|b| b.strip_prefix('-'))
                    .and_then(|b| b.parse().ok())
                    .unwrap_or(0);
            }
            continue;
        }
        let (state, path) = match rec.as_bytes()[0] {
            b'1' => {
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                match f.as_slice() {
                    [_, xy, .., path] => (state_from_xy(xy.as_bytes(), false), *path),
                    _ => continue,
                }
            }
            b'2' => {
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                // l'ancien nom suit, comme enregistrement séparé
                let _ = records.next();
                match f.as_slice() {
                    [_, xy, .., path] => (state_from_xy(xy.as_bytes(), true), *path),
                    _ => continue,
                }
            }
            b'u' => {
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                match f.last() {
                    Some(path) => (FileState::Conflict, *path),
                    None => continue,
                }
            }
            b'?' => (FileState::Untracked, rec.get(2..).unwrap_or("")),
            b'!' => (FileState::Ignored, rec.get(2..).unwrap_or("")),
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        let full = absolute(root, path);
        if state != FileState::Ignored {
            st.changes += 1;
            for dir in full.ancestors().skip(1) {
                // jusqu'à la racine comprise ; un dossier déjà noté arrête la remontée
                if !st.dirty_dirs.insert(dir.to_path_buf()) || dir == root {
                    break;
                }
            }
        }
        st.files.insert(full, state);
    }
    st.branch = match head.as_str() {
        "(detached)" => format!("({})", oid.get(..7).unwrap_or("?")),
        "" => String::new(),
        name => name.to_owned(),
    };
    st
}

/// Racine du dépôt Git qui contient `start` (fichier ou dossier) : le premier dossier, en
/// remontant, qui a un `.git` (dossier, ou fichier pour un sous-module ou un worktree).
#[must_use]
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Lance `git` dans `root` avec `args` ; sa sortie standard, ou `None` s'il n'est pas installé, échoue
/// ou dépasse `timeout`.
fn run_git(root: &Path, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // pas de fenêtre de console qui clignote
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // la sortie est lue pendant que le processus tourne : un tube plein le bloquerait
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(4)),
            Err(_) => return None,
        }
    }
}

/// L'état Git du dossier (ou du fichier) `start` ; `None` hors d'un dépôt, ou sans `git`. Bloquant.
#[must_use]
pub fn read_status(start: &Path) -> Option<GitStatus> {
    let root = find_repo_root(start)?;
    let out = run_git(
        &root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--ignored",
            "--untracked-files=normal",
        ],
        TIMEOUT,
    )?;
    Some(parse_status(&root, &String::from_utf8_lossy(&out)))
}

/// Réponse du thread Git.
#[derive(Debug)]
pub struct GitResult {
    /// Le dossier pour lequel l'état a été demandé.
    pub dir: PathBuf,
    pub status: Option<GitStatus>,
}

/// Démarre le thread Git. Chaque chemin envoyé demande l'état du dépôt qui le contient ;
/// `notify` reçoit les résultats (depuis ce thread).
pub fn spawn(notify: impl Fn(GitResult) + Send + 'static) -> Sender<PathBuf> {
    let (tx, rx) = unbounded::<PathBuf>();
    std::thread::Builder::new()
        .name("paulwrite-git".into())
        .spawn(move || {
            while let Ok(mut dir) = rx.recv() {
                // les demandes accumulées pendant une exécution : seule la dernière compte
                while let Ok(newer) = rx.try_recv() {
                    dir = newer;
                }
                let started = Instant::now();
                let status = read_status(&dir);
                notify(GitResult { dir, status });
                if let Some(wait) = MIN_SPACING.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        })
        .expect("thread git");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(r"C:\proj")
    }

    /// Un enregistrement par élément, terminé par un octet nul comme dans la vraie sortie.
    fn out(records: &[&str]) -> String {
        records.iter().map(|r| format!("{r}\0")).collect()
    }

    #[test]
    fn branche_avance_retard_et_fichiers() {
        let s = parse_status(
            &root(),
            &out(&[
                "# branch.oid 1234567890abcdef",
                "# branch.head main",
                "# branch.upstream origin/main",
                "# branch.ab +2 -1",
                "1 .M N... 100644 100644 100644 aaaa bbbb src/main.rs",
                "1 A. N... 000000 100644 100644 0000 cccc src/nouveau.rs",
                "1 .D N... 100644 100644 000000 dddd eeee vieux.txt",
                "? notes.txt",
                "? dossier_neuf/",
                "! target/",
            ]),
        );
        assert_eq!((s.branch.as_str(), s.ahead, s.behind), ("main", 2, 1));
        assert_eq!(s.changes, 5, "l'ignoré ne compte pas");
        let p = |rel: &str| absolute(&root(), rel);
        assert_eq!(s.state_of(&p("src/main.rs")), Some(FileState::Modified));
        assert_eq!(s.state_of(&p("src/nouveau.rs")), Some(FileState::Added));
        assert_eq!(s.state_of(&p("vieux.txt")), Some(FileState::Deleted));
        assert_eq!(s.state_of(&p("notes.txt")), Some(FileState::Untracked));
        assert_eq!(s.state_of(&p("README.md")), None);
        // un dossier ignoré ou non suivi : ses fichiers en héritent
        assert_eq!(
            s.state_of(&p("target/debug/x.exe")),
            Some(FileState::Ignored)
        );
        assert_eq!(s.state_of(&p("target")), Some(FileState::Ignored));
        assert_eq!(
            s.state_of(&p("dossier_neuf/a/b.txt")),
            Some(FileState::Untracked)
        );
        // dossiers qui contiennent des changements : la racine, src, dossier_neuf — pas target
        assert!(s.dir_dirty(&root()));
        assert!(s.dir_dirty(&p("src")));
        assert!(!s.dir_dirty(&p("target")));
        assert!(!s.dir_dirty(&p("docs")));
    }

    #[test]
    fn renommage_conflit_tete_detachee_et_sans_serveur() {
        let s = parse_status(
            &root(),
            &out(&[
                "# branch.oid abcdef0123456789",
                "# branch.head (detached)",
                // renommage : l'ancien nom est un enregistrement séparé qu'il faut sauter
                "2 R. N... 100644 100644 100644 aaaa bbbb R100 nouveau.rs",
                "ancien.rs",
                "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc fusion.txt",
                "1 MM N... 100644 100644 100644 aaaa bbbb deux.txt",
            ]),
        );
        assert_eq!(s.branch, "(abcdef0)");
        assert_eq!((s.ahead, s.behind), (0, 0));
        let p = |rel: &str| absolute(&root(), rel);
        assert_eq!(s.state_of(&p("nouveau.rs")), Some(FileState::Renamed));
        assert_eq!(
            s.state_of(&p("ancien.rs")),
            None,
            "l'ancien nom n'est pas un fichier"
        );
        assert_eq!(s.state_of(&p("fusion.txt")), Some(FileState::Conflict));
        assert_eq!(s.state_of(&p("deux.txt")), Some(FileState::Modified));
        assert_eq!(s.changes, 3);
        // pas de sortie : aucun changement, pas de plantage
        let vide = parse_status(&root(), "");
        assert_eq!((vide.changes, vide.branch.as_str()), (0, ""));
    }

    #[test]
    fn racine_du_depot() {
        let dir = std::env::temp_dir().join(format!("paulwrite-git-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        assert_eq!(find_repo_root(&dir.join("a/b")), None);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(find_repo_root(&dir.join("a/b")), Some(dir.clone()));
        // un fichier `.git` (sous-module, worktree) compte aussi
        std::fs::write(dir.join("a/.git"), "gitdir: ../x").unwrap();
        assert_eq!(find_repo_root(&dir.join("a/b")), Some(dir.join("a")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Avec un vrai dépôt (si `git` est installé) : de bout en bout, comme l'éditeur l'appelle.
    #[test]
    fn vrai_depot() {
        let Some(_) = run_git(Path::new("."), &["--version"], Duration::from_secs(10)) else {
            return; // git absent : rien à vérifier ici
        };
        let dir = std::env::temp_dir().join(format!("paulwrite-git-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let git = |args: &[&str]| {
            run_git(&dir, args, Duration::from_secs(30)).expect("git");
        };
        git(&["init", "-q", "-b", "principale"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "journal.log\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "premier"]);
        // modifié, non suivi, ignoré
        std::fs::write(dir.join("src/a.rs"), "fn a() { 1; }\n").unwrap();
        std::fs::write(dir.join("neuf.txt"), "x\n").unwrap();
        std::fs::write(dir.join("journal.log"), "x\n").unwrap();
        let s = read_status(&dir.join("src")).expect("dépôt trouvé depuis un sous-dossier");
        assert_eq!(s.branch, "principale");
        assert_eq!(s.root, dir);
        let p = |rel: &str| absolute(&dir, rel);
        assert_eq!(s.state_of(&p("src/a.rs")), Some(FileState::Modified));
        assert_eq!(s.state_of(&p("neuf.txt")), Some(FileState::Untracked));
        assert_eq!(s.state_of(&p("journal.log")), Some(FileState::Ignored));
        assert_eq!(s.state_of(&p("b.txt")), None);
        assert!(s.dir_dirty(&p("src")));
        // hors dépôt : rien
        let absent = std::env::temp_dir().join("paulwrite-absent-xyz");
        if find_repo_root(&absent).is_none() {
            assert!(read_status(&absent).is_none());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
