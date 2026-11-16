//! Thread de travail : toutes les E/S fichier et les boîtes de dialogue bloquantes.
//! Le thread UI n'en fait jamais (règle de conception n°1) : il envoie un [`Job`] par canal
//! et reçoit un [`Done`] sous forme d'événement utilisateur.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use paulwrite_core::io::{self, LoadOptions};
use paulwrite_core::{Buffer, Document, Encoding};
use paulwrite_render::platform;
use ropey::Rope;

/// Travail demandé au worker.
pub enum Job {
    /// Lire et décoder un fichier.
    Open(PathBuf),
    /// Boîte « Ouvrir » (bloquante).
    PickOpen { owner: isize },
    /// Boîte « Enregistrer sous » (bloquante).
    PickSave { owner: isize, suggested: String },
    /// Écriture atomique ; `rope` est un clone O(1) du contenu au moment de la demande.
    Save {
        path: PathBuf,
        rope: Rope,
        encoding: Encoding,
        revision: u64,
    },
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Job::Open(p) => write!(f, "Open({p:?})"),
            Job::PickOpen { .. } => write!(f, "PickOpen"),
            Job::PickSave { .. } => write!(f, "PickSave"),
            Job::Save { path, .. } => write!(f, "Save({path:?})"),
        }
    }
}

/// Résultat renvoyé au thread UI.
pub enum Done {
    Loaded {
        path: PathBuf,
        result: Result<Document, String>,
    },
    Progress {
        path: PathBuf,
        done: u64,
        total: u64,
    },
    Saved {
        path: PathBuf,
        revision: u64,
        result: Result<(), String>,
    },
    OpenPicked(Option<PathBuf>),
    SavePicked(Option<PathBuf>),
}

impl std::fmt::Debug for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Done::Loaded { path, result } => write!(f, "Loaded({path:?}, ok={})", result.is_ok()),
            Done::Progress { done, total, .. } => write!(f, "Progress({done}/{total})"),
            Done::Saved { path, result, .. } => write!(f, "Saved({path:?}, ok={})", result.is_ok()),
            Done::OpenPicked(p) => write!(f, "OpenPicked({p:?})"),
            Done::SavePicked(p) => write!(f, "SavePicked({p:?})"),
        }
    }
}

/// Charge un fichier en document (appelé sur le worker).
fn load(path: &Path, notify: &dyn Fn(Done)) -> Result<Document, String> {
    let mut last = Instant::now();
    let mut progress = |done: u64, total: u64| {
        if last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            notify(Done::Progress {
                path: path.to_path_buf(),
                done,
                total,
            });
        }
        true
    };
    let loaded = io::load_file(path, &LoadOptions::default(), Some(&mut progress))
        .map_err(|e| e.to_string())?;
    Ok(Document::from_buffer(
        Buffer::from_rope(loaded.rope),
        loaded.encoding,
        loaded.line_ending,
    ))
}

/// Démarre le worker. `notify` est appelé **depuis le worker** pour chaque résultat (dans
/// l'application : envoi d'un événement utilisateur au thread UI).
pub fn spawn(notify: impl Fn(Done) + Send + 'static) -> Sender<Job> {
    let (tx, rx) = unbounded::<Job>();
    thread::Builder::new()
        .name("paulwrite-worker".into())
        .spawn(move || {
            for job in rx {
                match job {
                    Job::Open(path) => {
                        let result = load(&path, &notify);
                        notify(Done::Loaded { path, result });
                    }
                    Job::PickOpen { owner } => {
                        notify(Done::OpenPicked(platform::pick_open_file(owner)))
                    }
                    Job::PickSave { owner, suggested } => {
                        notify(Done::SavePicked(platform::pick_save_file(
                            owner, &suggested,
                        )));
                    }
                    Job::Save {
                        path,
                        rope,
                        encoding,
                        revision,
                    } => {
                        let result =
                            io::save_file(&path, &rope, encoding).map_err(|e| e.to_string());
                        notify(Done::Saved {
                            path,
                            revision,
                            result,
                        });
                    }
                }
            }
        })
        .expect("thread worker");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn ouvrir_enregistrer_et_relire() {
        let dir = std::env::temp_dir().join(format!("paulwrite-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "bonjour\r\nmonde").unwrap();

        let (out_tx, out_rx) = mpsc::channel();
        let out_tx = std::sync::Mutex::new(out_tx);
        let jobs = spawn(move |d| {
            let _ = out_tx.lock().unwrap().send(d);
        });

        jobs.send(Job::Open(path.clone())).unwrap();
        let doc = loop {
            if let Done::Loaded { result, .. } =
                out_rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                break result.unwrap();
            }
        };
        assert_eq!(doc.buffer().rope().to_string(), "bonjour\r\nmonde");
        assert_eq!(doc.line_ending(), paulwrite_core::LineEnding::CrLf);

        let out = dir.join("b.txt");
        jobs.send(Job::Save {
            path: out.clone(),
            rope: doc.buffer().rope().clone(),
            encoding: doc.encoding(),
            revision: 7,
        })
        .unwrap();
        loop {
            if let Done::Saved {
                revision, result, ..
            } = out_rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert_eq!(revision, 7);
                result.unwrap();
                break;
            }
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "bonjour\r\nmonde");

        // fichier inexistant : erreur, pas de panique
        jobs.send(Job::Open(dir.join("absent.txt"))).unwrap();
        loop {
            if let Done::Loaded { result, .. } =
                out_rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert!(result.is_err());
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
