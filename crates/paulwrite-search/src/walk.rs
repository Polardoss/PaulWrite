//! Liste des fichiers d'un projet : parcours parallèle qui respecte `.gitignore`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Sender;
use ignore::{WalkBuilder, WalkState};

/// Taille d'un lot de chemins envoyé dans le canal.
const BATCH: usize = 1024;

/// Réglages du parcours.
#[derive(Clone, Debug, Default)]
pub struct IndexOptions {
    /// Inclure les fichiers et dossiers cachés (commençant par un point).
    pub hidden: bool,
}

/// Lot de chemins vidé dans le canal quand il est plein ou quand le thread de parcours se termine.
struct Batcher {
    tx: Sender<Vec<String>>,
    buf: Vec<String>,
}

impl Batcher {
    fn push(&mut self, path: String) {
        self.buf.push(path);
        if self.buf.len() >= BATCH {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            let _ = self.tx.send(std::mem::take(&mut self.buf));
        }
    }
}

impl Drop for Batcher {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Liste les fichiers sous `root` (chemins relatifs à `root`), par lots de 1024, dans `tx`.
/// Respecte `.gitignore`, `.ignore` et les fichiers cachés. Rend la main quand le parcours est
/// terminé ou annulé (`cancel`). Renvoie le nombre de fichiers listés.
pub fn list_files(
    root: &Path,
    opts: &IndexOptions,
    cancel: &AtomicBool,
    tx: Sender<Vec<String>>,
) -> usize {
    let count = std::sync::atomic::AtomicUsize::new(0);
    WalkBuilder::new(root)
        .hidden(!opts.hidden)
        // `.gitignore` s'applique même hors d'un dépôt git (un dossier de projet quelconque).
        .require_git(false)
        .build_parallel()
        .run(|| {
            let mut batcher = Batcher {
                tx: tx.clone(),
                buf: Vec::with_capacity(BATCH),
            };
            let count = &count;
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let path = entry.path().strip_prefix(root).unwrap_or(entry.path());
                batcher.push(path.to_string_lossy().into_owned());
                count.fetch_add(1, Ordering::Relaxed);
                WalkState::Continue
            })
        });
    count.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    pub fn make_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("src/sub")).unwrap();
        fs::create_dir_all(r.join("target/debug")).unwrap();
        fs::create_dir_all(r.join(".hidden_dir")).unwrap();
        fs::write(r.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(r.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(r.join("src/sub/lib.rs"), "pub fn needle() {}\n").unwrap();
        fs::write(r.join("README.md"), "# Projet\n").unwrap();
        fs::write(r.join("debug.log"), "ignoré\n").unwrap();
        fs::write(r.join("target/debug/out.bin"), "ignoré\n").unwrap();
        fs::write(r.join(".hidden_dir/secret.txt"), "caché\n").unwrap();
        dir
    }

    fn collect(root: &Path, opts: &IndexOptions) -> Vec<String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = list_files(root, opts, &AtomicBool::new(false), tx);
        let mut all: Vec<String> = rx.iter().flatten().map(|p| p.replace('\\', "/")).collect();
        all.sort();
        assert_eq!(n, all.len());
        all
    }

    #[test]
    fn respecte_gitignore_et_fichiers_caches() {
        let dir = make_project();
        let all = collect(dir.path(), &IndexOptions::default());
        // .gitignore lui-même est caché ; target/ et *.log sont ignorés
        assert_eq!(all, ["README.md", "src/main.rs", "src/sub/lib.rs"]);

        let with_hidden = collect(dir.path(), &IndexOptions { hidden: true });
        assert!(with_hidden.contains(&".hidden_dir/secret.txt".to_owned()));
        assert!(with_hidden.contains(&".gitignore".to_owned()));
        assert!(!with_hidden.iter().any(|p| p.starts_with("target/")));
    }

    #[test]
    fn annulation_immediate() {
        let dir = make_project();
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = list_files(
            dir.path(),
            &IndexOptions::default(),
            &AtomicBool::new(true),
            tx,
        );
        assert_eq!(n, 0);
        assert_eq!(rx.iter().count(), 0);
    }

    #[test]
    fn dossier_inexistant() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let n = list_files(
            Path::new("Z:\\n'existe\\pas"),
            &IndexOptions::default(),
            &AtomicBool::new(false),
            tx,
        );
        assert_eq!(n, 0);
        assert_eq!(rx.iter().count(), 0);
    }
}

#[cfg(test)]
pub(crate) use tests::make_project;
