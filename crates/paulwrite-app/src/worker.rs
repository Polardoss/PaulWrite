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

use crate::i18n::Lang;
use ropey::Rope;

use crate::config::Config;
use crate::sidebar::{is_hidden_dir, sort_entries, Entry};

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
        lang: Lang,
    },
    /// Affiche un message d'erreur (sans réponse).
    ShowError { owner: isize, text: String },
    /// Lire et décoder un fichier pour l'onglet `tab`.
    Open { tab: TabId, path: PathBuf },
    /// Boîte « Ouvrir » (bloquante).
    PickOpen { owner: isize, lang: Lang },
    /// Boîte « Ouvrir un dossier » (bloquante).
    PickFolder { owner: isize },
    /// Liste le contenu d'un dossier (barre latérale).
    ListDir { path: PathBuf },
    /// Boîte « Enregistrer sous » (bloquante) pour l'onglet `tab`.
    PickSave {
        tab: TabId,
        owner: isize,
        suggested: String,
        lang: Lang,
    },
    /// Lance une nouvelle fenêtre : un nouveau processus `paulwrite`, sans document.
    NewWindow,
    /// Enregistre un réglage dans `config.toml` (clé et valeur texte) ; le fichier est créé au
    /// besoin, le reste de son contenu est conservé.
    SaveSetting { key: &'static str, value: String },
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
            Job::PickFolder { .. } => write!(f, "PickFolder"),
            Job::ListDir { path } => write!(f, "ListDir({path:?})"),
            Job::PickSave { tab, .. } => write!(f, "PickSave({tab})"),
            Job::SaveSetting { key, .. } => write!(f, "SaveSetting({key})"),
            Job::NewWindow => write!(f, "NewWindow"),
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
    FolderPicked(Option<PathBuf>),
    /// Contenu d'un dossier (déjà trié) ; vide si la lecture a échoué.
    DirListed {
        path: PathBuf,
        entries: Vec<Entry>,
    },
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
            Done::FolderPicked(p) => write!(f, "FolderPicked({p:?})"),
            Done::DirListed { path, entries } => {
                write!(f, "DirListed({path:?}, {} éléments)", entries.len())
            }
            Done::SavePicked { tab, path } => write!(f, "SavePicked({tab}, {path:?})"),
        }
    }
}

/// Contenu d'un dossier, trié (appelé sur le worker). Vide si la lecture échoue.
pub fn list_dir(path: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // `file_type` vient de l'énumération du dossier (aucun appel au système par entrée) ;
            // seul un lien symbolique est suivi, pour savoir s'il pointe vers un dossier.
            let kind = e.file_type().ok()?;
            let is_dir = if kind.is_symlink() {
                e.path().is_dir()
            } else {
                kind.is_dir()
            };
            (!(is_dir && is_hidden_dir(&name))).then_some(Entry { name, is_dir })
        })
        .collect();
    sort_entries(&mut entries);
    entries
}

/// Contenu d'un fichier de configuration neuf (tout est commenté : les valeurs par défaut
/// s'appliquent).
const CONFIG_TEMPLATE: &str =
    "# Configuration de PaulWrite (une valeur par ligne : clé = valeur)\n\
# highlight = true                # false : pas de coloration syntaxique\n\
# highlight_max_bytes = 1048576   # au-delà de cette taille (octets), pas de coloration\n\
# lsp = false                     # true : serveurs de langage (programmes externes)\n\
# lsp_rust = \"rust-analyzer\"\n\
# language = \"auto\"              # \"en\", \"fr\" ou \"auto\" (langue de Windows)\n\
# word_wrap = true                # false : pas de retour \u{e0} la ligne automatique\n";

/// Écrit `key = "value"` dans `config.toml` (appelé sur le worker).
fn save_setting(key: &str, value: &str) -> std::io::Result<()> {
    let Some(path) = Config::path() else {
        return Ok(());
    };
    let current = std::fs::read_to_string(&path).unwrap_or_else(|_| CONFIG_TEMPLATE.to_owned());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, Config::with_setting(&current, key, value))
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
                    Job::ConfirmSave {
                        owner,
                        name,
                        for_,
                        lang,
                    } => notify(Done::SaveConfirmed {
                        for_,
                        choice: platform::confirm_save(
                            owner,
                            &lang.pick(
                                format!("Save changes to \u{ab} {name} \u{bb}?"),
                                format!("Enregistrer les modifications de \u{ab} {name} \u{bb} ?"),
                            ),
                        ),
                    }),
                    Job::ShowError { owner, text } => platform::show_error(owner, &text),
                    Job::Open { tab, path } => {
                        // Le fichier de configuration est créé à la première ouverture.
                        if !path.exists() && Config::path().as_deref() == Some(path.as_path()) {
                            if let Some(dir) = path.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            let _ = std::fs::write(&path, CONFIG_TEMPLATE);
                        }
                        if path.is_dir() {
                            notify(Done::Directory { tab, path });
                        } else {
                            let result = load(tab, &path, &notify);
                            notify(Done::Loaded { tab, path, result });
                        }
                    }
                    Job::PickOpen { owner, lang } => {
                        let all = lang.pick("All files", "Tous les fichiers");
                        notify(Done::OpenPicked(platform::pick_open_file(owner, all)));
                    }
                    Job::PickFolder { owner } => {
                        notify(Done::FolderPicked(platform::pick_folder(owner)));
                    }
                    Job::ListDir { path } => {
                        let entries = list_dir(&path);
                        notify(Done::DirListed { path, entries });
                    }
                    Job::PickSave {
                        tab,
                        owner,
                        suggested,
                        lang,
                    } => notify(Done::SavePicked {
                        tab,
                        path: platform::pick_save_file(
                            owner,
                            &suggested,
                            lang.pick("All files", "Tous les fichiers"),
                        ),
                    }),
                    Job::NewWindow => {
                        // un processus de plus : l'état (onglets, curseurs) n'est pas partagé
                        if let Err(e) = std::env::current_exe()
                            .and_then(|exe| std::process::Command::new(exe).spawn().map(|_| ()))
                        {
                            eprintln!("nouvelle fen\u{ea}tre impossible : {e}");
                        }
                    }
                    Job::SaveSetting { key, value } => {
                        if let Err(e) = save_setting(key, &value) {
                            eprintln!("r\u{e9}glage {key} non enregistr\u{e9} : {e}");
                        }
                    }
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
