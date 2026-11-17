//! Thread de travail : toutes les E/S fichier et les boîtes de dialogue bloquantes.
//! Le thread UI n'en fait jamais (règle de conception n°1) : il envoie un [`Job`] par canal
//! et reçoit un [`Done`] sous forme d'événement utilisateur.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use paulwrite_core::io::{self, LoadOptions};
use paulwrite_core::{Buffer, Document, Encoding};
use paulwrite_render::platform::{self, SaveChoice};
use ropey::Rope;

use crate::config::Config;

/// Identifiant d'onglet (stable pendant la vie de l'onglet).
pub type TabId = u64;

/// Ce que l'application fera de la réponse à « Enregistrer les modifications ? ».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmFor {
    /// Fermeture d'un onglet.
    CloseTab(TabId),
    /// Fermeture de l'application, pour cet onglet modifié.
    Quit(TabId),
}

/// Travail demandé au worker.
///
/// Les boîtes de dialogue modales (fichiers, confirmation, erreur) s'affichent **ici et jamais
/// dans la boucle d'événements** : winit n'autorise pas la ré-entrance, et une boîte modale ouverte
/// pendant le traitement d'un événement provoque un redessin imbriqué qui fait planter le programme.
pub enum Job {
    /// « Enregistrer les modifications de … ? » ; la réponse revient en [`Done::SaveConfirmed`].
    ConfirmSave {
        owner: isize,
        name: String,
        for_: ConfirmFor,
    },
    /// Affiche un message d'erreur (sans réponse).
    ShowError { owner: isize, text: String },
    /// Lire et décoder un fichier pour l'onglet `tab`.
    Open { tab: TabId, path: PathBuf },
    /// Boîte « Ouvrir » (bloquante).
    PickOpen { owner: isize },
    /// Boîte « Enregistrer sous » (bloquante) pour l'onglet `tab`.
    PickSave {
        tab: TabId,
        owner: isize,
        suggested: String,
    },
    /// Écriture atomique ; `rope` est un clone O(1) du contenu au moment de la demande.
    Save {
        tab: TabId,
        path: PathBuf,
        rope: Rope,
        encoding: Encoding,
        revision: u64,
    },
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Job::ConfirmSave { for_, .. } => write!(f, "ConfirmSave({for_:?})"),
            Job::ShowError { .. } => write!(f, "ShowError"),
            Job::Open { tab, path } => write!(f, "Open({tab}, {path:?})"),
            Job::PickOpen { .. } => write!(f, "PickOpen"),
            Job::PickSave { tab, .. } => write!(f, "PickSave({tab})"),
            Job::Save { tab, path, .. } => write!(f, "Save({tab}, {path:?})"),
        }
    }
}

/// Résultat renvoyé au thread UI.
pub enum Done {
    /// Configuration lue au démarrage du worker (toujours le premier message).
    Config(Config),
    /// Réponse de l'utilisateur à [`Job::ConfirmSave`].
    SaveConfirmed {
        for_: ConfirmFor,
        choice: SaveChoice,
    },
    Loaded {
        tab: TabId,
        path: PathBuf,
        result: Result<Document, String>,
    },
    /// Le chemin demandé est un dossier : il devient la racine du projet.
    Directory {
        tab: TabId,
        path: PathBuf,
    },
    Progress {
        tab: TabId,
        done: u64,
        total: u64,
    },
    Saved {
        tab: TabId,
        path: PathBuf,
        revision: u64,
        result: Result<(), String>,
    },
    OpenPicked(Option<PathBuf>),
    SavePicked {
        tab: TabId,
        path: Option<PathBuf>,
    },
}

impl std::fmt::Debug for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Done::Config(c) => write!(f, "Config({c:?})"),
            Done::SaveConfirmed { for_, choice } => {
                write!(f, "SaveConfirmed({for_:?}, {choice:?})")
            }
            Done::Loaded { tab, path, result } => {
                write!(f, "Loaded({tab}, {path:?}, ok={})", result.is_ok())
            }
            Done::Directory { tab, path } => write!(f, "Directory({tab}, {path:?})"),
            Done::Progress { done, total, .. } => write!(f, "Progress({done}/{total})"),
            Done::Saved {
                tab, path, result, ..
            } => write!(f, "Saved({tab}, {path:?}, ok={})", result.is_ok()),
            Done::OpenPicked(p) => write!(f, "OpenPicked({p:?})"),
            Done::SavePicked { tab, path } => write!(f, "SavePicked({tab}, {path:?})"),
        }
    }
}

/// Charge un fichier en document (appelé sur le worker).
fn load(tab: TabId, path: &Path, notify: &dyn Fn(Done)) -> Result<Document, String> {
    let mut last = Instant::now();
    let mut progress = |done: u64, total: u64| {
        if last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            notify(Done::Progress { tab, done, total });
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
/// l'application : envoi d'un événement utilisateur au thread UI). Le premier message est
/// [`Done::Config`] : la configuration est lue ici, hors du thread UI.
pub fn spawn(notify: impl Fn(Done) + Send + 'static) -> Sender<Job> {
    let (tx, rx) = unbounded::<Job>();
    thread::Builder::new()
        .name("paulwrite-worker".into())
        .spawn(move || {
            notify(Done::Config(Config::load()));
            for job in rx {
                match job {
                    Job::ConfirmSave { owner, name, for_ } => notify(Done::SaveConfirmed {
                        for_,
                        choice: platform::confirm_save(owner, &name),
                    }),
                    Job::ShowError { owner, text } => platform::show_error(owner, &text),
                    Job::Open { tab, path } => {
                        if path.is_dir() {
                            notify(Done::Directory { tab, path });
                        } else {
                            let result = load(tab, &path, &notify);
                            notify(Done::Loaded { tab, path, result });
                        }
                    }
                    Job::PickOpen { owner } => {
                        notify(Done::OpenPicked(platform::pick_open_file(owner)));
                    }
                    Job::PickSave {
                        tab,
                        owner,
                        suggested,
                    } => notify(Done::SavePicked {
                        tab,
                        path: platform::pick_save_file(owner, &suggested),
                    }),
                    Job::Save {
                        tab,
                        path,
                        rope,
                        encoding,
                        revision,
                    } => {
                        let result =
                            io::save_file(&path, &rope, encoding).map_err(|e| e.to_string());
                        notify(Done::Saved {
                            tab,
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
        // la configuration arrive en premier
        assert!(matches!(
            out_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            Done::Config(_)
        ));

        jobs.send(Job::Open {
            tab: 3,
            path: path.clone(),
        })
        .unwrap();
        let doc = loop {
            if let Done::Loaded { tab, result, .. } =
                out_rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert_eq!(tab, 3);
                break result.unwrap();
            }
        };
        assert_eq!(doc.buffer().rope().to_string(), "bonjour\r\nmonde");
        assert_eq!(doc.line_ending(), paulwrite_core::LineEnding::CrLf);

        let out = dir.join("b.txt");
        jobs.send(Job::Save {
            tab: 3,
            path: out.clone(),
            rope: doc.buffer().rope().clone(),
            encoding: doc.encoding(),
            revision: 7,
        })
        .unwrap();
        loop {
            if let Done::Saved {
                tab,
                revision,
                result,
                ..
            } = out_rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert_eq!((tab, revision), (3, 7));
                result.unwrap();
                break;
            }
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "bonjour\r\nmonde");

        // fichier inexistant : erreur, pas de panique
        jobs.send(Job::Open {
            tab: 4,
            path: dir.join("absent.txt"),
        })
        .unwrap();
        loop {
            if let Done::Loaded { tab, result, .. } =
                out_rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert_eq!(tab, 4);
                assert!(result.is_err());
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
