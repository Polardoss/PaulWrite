//! Tests du client LSP contre le faux serveur `paulwrite-fake-lsp` (qui parle réellement le
//! protocole et reconstruit le texte des documents qu'on lui envoie).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use paulwrite_core::Buffer;
use paulwrite_lsp::{spawn, Event, Request, ServerConfig, Severity};
use ropey::Rope;

const TIMEOUT: Duration = Duration::from_secs(20);

struct Harness {
    tx: Sender<Request>,
    rx: mpsc::Receiver<Event>,
    path: PathBuf,
    _dir: tempfile::TempDir,
    next_id: u64,
}

fn start(server_args: &[&str], language: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut configs = HashMap::new();
    configs.insert(
        "fake".to_owned(),
        ServerConfig {
            command: env!("CARGO_BIN_EXE_paulwrite-fake-lsp").to_owned(),
            args: server_args.iter().map(|s| (*s).to_owned()).collect(),
        },
    );
    configs.insert(
        "absent".to_owned(),
        ServerConfig {
            command: "serveur-de-langage-qui-n-existe-pas".to_owned(),
            args: Vec::new(),
        },
    );
    let (otx, rx) = mpsc::channel();
    let otx = Mutex::new(otx);
    let tx = spawn(configs, move |e| {
        let _ = otx.lock().unwrap().send(e);
    });
    let path = dir.path().join("a.rs");
    let _ = language;
    Harness {
        tx,
        rx,
        path,
        _dir: dir,
        next_id: 0,
    }
}

impl Harness {
    fn open(&self, language: &str, text: &str) {
        self.tx
            .send(Request::Open {
                path: self.path.clone(),
                language: language.to_owned(),
                root: None,
                version: 1,
                text: Rope::from_str(text),
            })
            .unwrap();
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Attend un événement qui satisfait `pred` (les autres sont ignorés).
    fn wait<T>(&self, mut pred: impl FnMut(Event) -> Option<T>) -> T {
        let end = Instant::now() + TIMEOUT;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(e) => {
                    if let Some(v) = pred(e) {
                        return v;
                    }
                }
                Err(_) => panic!("événement attendu non reçu"),
            }
        }
    }

    /// Survol sans le marqueur ` [cfg ok]` : celui-ci dépend de l'instant où le serveur reçoit la
    /// réponse du client à sa requête de configuration, pas de la synchronisation du texte.
    fn hover_text(&mut self, line: usize, col: usize) -> String {
        let text = self.hover(line, col).expect("survol");
        text.trim_end_matches(" [cfg ok]").to_owned()
    }

    fn hover(&mut self, line: usize, col: usize) -> Option<String> {
        let id = self.id();
        self.tx
            .send(Request::Hover {
                id,
                path: self.path.clone(),
                line,
                col16: col,
            })
            .unwrap();
        self.wait(|e| match e {
            Event::Hover { id: i, text } if i == id => Some(text),
            _ => None,
        })
    }
}

#[test]
fn demarrage_et_diagnostics_en_utf16() {
    let h = start(&[], "fake");
    // « é😀 » avant ERROR : colonne UTF-16 = 1 + 2 + 1 = 4 ; l'avertissement suit
    h.open("fake", "fn é😀 ERROR() {}\nlet WARN = 1;\n");
    h.wait(|e| matches!(e, Event::ServerStarted { language } if language == "fake").then_some(()));
    let items = h.wait(|e| match e {
        Event::Diagnostics { path, items } if path == h.path => Some(items),
        _ => None,
    });
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0].severity, Severity::Error);
    assert_eq!((items[0].start, items[0].end), ((0, 7), (0, 12)));
    assert_eq!(items[1].severity, Severity::Warning);
    assert_eq!(items[1].start, (1, 4));
    assert!(items[1].message.contains("WARN"));
}

#[test]
fn le_client_repond_aux_requetes_du_serveur() {
    let mut h = start(&[], "fake");
    h.open("fake", "abc\n");
    // le serveur envoie `workspace/configuration` après `initialized` : sans réponse correcte du
    // client, le survol ne porterait pas la marque [cfg ok]
    // la réponse du client part en parallèle de nos requêtes : on réessaie jusqu'à voir le marqueur
    let end = Instant::now() + TIMEOUT;
    loop {
        let text = h.hover(0, 0).unwrap();
        if text == "ligne 0: abc [cfg ok]" {
            break;
        }
        assert_eq!(text, "ligne 0: abc", "texte inattendu");
        assert!(
            Instant::now() < end,
            "le client n'a jamais répondu à workspace/configuration"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn completion_survol_et_definition() {
    let mut h = start(&[], "fake");
    h.open("fake", "fn calcul() {}\nlet x = calcul();\n");
    let id = h.id();
    h.tx.send(Request::Completion {
        id,
        path: h.path.clone(),
        line: 1,
        col16: 9,
    })
    .unwrap();
    let items = h.wait(|e| match e {
        Event::Completion { id: i, items } if i == id => Some(items),
        _ => None,
    });
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["alpha", "beta", "pos:1:9"],
        "la position doit être transmise telle quelle"
    );
    assert_eq!(
        (items[1].insert_text.as_str(), items[1].detail.as_str()),
        ("beta()", "fonction")
    );
    assert_eq!(items[0].insert_text, "alpha");

    let id = h.id();
    h.tx.send(Request::Definition {
        id,
        path: h.path.clone(),
        line: 1,
        col16: 10,
    })
    .unwrap();
    let locations = h.wait(|e| match e {
        Event::Definition { id: i, locations } if i == id => Some(locations),
        _ => None,
    });
    assert_eq!(locations.len(), 1);
    assert_eq!((locations[0].line, locations[0].col16), (0, 3));
    assert_eq!(
        paulwrite_lsp::path_key(&locations[0].path),
        paulwrite_lsp::path_key(&h.path)
    );
    // aucun symbole : réponse vide, pas d'erreur
    let id = h.id();
    h.tx.send(Request::Definition {
        id,
        path: h.path.clone(),
        line: 1,
        col16: 0,
    })
    .unwrap();
    let locations = h.wait(|e| match e {
        Event::Definition { id: i, locations } if i == id => Some(locations),
        _ => None,
    });
    assert!(locations.is_empty());
}

/// Après des modifications aléatoires journalisées par le buffer, le texte reconstruit par le
/// serveur doit être identique au nôtre, ligne par ligne (accents, émojis, sauts de ligne).
fn synchronisation_aleatoire(server_args: &[&str], seed: u64) {
    let mut h = start(server_args, "fake");
    let mut buf = Buffer::from_text("fn é() {}\nlet a = \"😀\";\n\nfin\n");
    buf.track_changes(true);
    h.open("fake", &buf.to_string());

    let mut rng = seed;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let snippets = ["a", "é", "😀", "\n", "xy\nz", " ERROR ", "fn ", "{}"];
    for round in 0..40 {
        // quelques modifications par lot, comme des frappes arrivées entre deux images
        for _ in 0..=(next() % 4) {
            let n = buf.len_chars();
            if next() % 3 != 0 || n == 0 {
                let at = (next() % (n as u64 + 1)) as usize;
                buf.insert(at, snippets[(next() % snippets.len() as u64) as usize]);
            } else {
                let s = (next() % n as u64) as usize;
                buf.delete(s..(s + (next() % 5) as usize).min(n));
            }
        }
        let version = 2 + round;
        h.tx.send(Request::Change {
            path: h.path.clone(),
            version,
            changes: buf.take_changes(),
            text: buf.rope().clone(),
        })
        .unwrap();
        // vérification d'une ligne choisie au hasard (les requêtes suivent les modifications)
        let line = (next() % buf.len_lines() as u64) as usize;
        let expected: String = buf
            .line(line)
            .to_string()
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        let got = h.hover_text(line, 0);
        assert_eq!(
            got,
            format!("ligne {line}: {expected}"),
            "synchronisation incorrecte au tour {round} ({server_args:?})"
        );
    }
    // et à la fin, toutes les lignes
    for line in 0..buf.len_lines() {
        let expected: String = buf
            .line(line)
            .to_string()
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        assert_eq!(h.hover_text(line, 0), format!("ligne {line}: {expected}"));
    }
}

#[test]
fn synchronisation_incrementale() {
    synchronisation_aleatoire(&[], 0x1234_5678_9abc_def1);
    synchronisation_aleatoire(&["--sync", "incremental"], 0x0fed_cba9_8765_4321);
}

#[test]
fn synchronisation_texte_entier() {
    synchronisation_aleatoire(&["--sync", "full"], 0x1111_2222_3333_4445);
}

#[test]
fn diagnostics_mis_a_jour_apres_modification() {
    let h = start(&[], "fake");
    h.open("fake", "ERROR\n");
    let first = h.wait(|e| match e {
        Event::Diagnostics { items, .. } => Some(items),
        _ => None,
    });
    assert_eq!(first.len(), 1);
    // on supprime « ERROR » (colonnes UTF-16 0..5)
    h.tx.send(Request::Change {
        path: h.path.clone(),
        version: 2,
        changes: vec![paulwrite_core::TextChange {
            start: (0, 0),
            end: (0, 5),
            text: String::new(),
        }],
        text: Rope::from_str("\n"),
    })
    .unwrap();
    let second = h.wait(|e| match e {
        Event::Diagnostics { items, .. } => Some(items),
        _ => None,
    });
    assert!(
        second.is_empty(),
        "plus d'erreur après la correction : {second:?}"
    );
}

#[test]
fn requetes_envoyees_avant_la_fin_du_demarrage() {
    // Ouverture, modification et survol envoyés d'un coup, avant même la réponse à `initialize` :
    // l'ordre doit être conservé (le survol voit la version modifiée).
    let mut h = start(&["--delay-ms", "0"], "fake");
    h.open("fake", "avant\n");
    h.tx.send(Request::Change {
        path: h.path.clone(),
        version: 2,
        changes: vec![paulwrite_core::TextChange {
            start: (0, 0),
            end: (0, 5),
            text: "apres".into(),
        }],
        text: Rope::from_str("apres\n"),
    })
    .unwrap();
    assert_eq!(h.hover_text(0, 0), "ligne 0: apres");
}

#[test]
fn langage_sans_serveur_et_serveur_introuvable() {
    let mut h = start(&[], "fake");
    // aucun serveur configuré pour ce langage
    h.open("inconnu", "x\n");
    let id = h.id();
    h.tx.send(Request::Hover {
        id,
        path: h.path.clone(),
        line: 0,
        col16: 0,
    })
    .unwrap();
    let reason = h.wait(|e| match e {
        Event::Unavailable { id: i, reason } if i == id => Some(reason),
        _ => None,
    });
    assert!(reason.contains("inconnu"), "{reason}");

    // serveur configuré mais introuvable
    let mut h = start(&[], "fake");
    h.open("absent", "x\n");
    let message = h.wait(|e| match e {
        Event::ServerError { language, message } if language == "absent" => Some(message),
        _ => None,
    });
    assert!(
        message.contains("serveur-de-langage-qui-n-existe-pas"),
        "{message}"
    );
    let id = h.id();
    h.tx.send(Request::Completion {
        id,
        path: h.path.clone(),
        line: 0,
        col16: 0,
    })
    .unwrap();
    h.wait(|e| matches!(e, Event::Unavailable { id: i, .. } if i == id).then_some(()));
}

#[test]
fn le_serveur_s_arrete_en_pleine_requete() {
    let mut h = start(&["--exit-on-completion"], "fake");
    h.open("fake", "abc\n");
    h.hover(0, 0); // le serveur est prêt
    let id = h.id();
    h.tx.send(Request::Completion {
        id,
        path: h.path.clone(),
        line: 0,
        col16: 0,
    })
    .unwrap();
    // la demande sans réponse est signalée, puis la sortie du serveur
    h.wait(|e| matches!(e, Event::Unavailable { id: i, .. } if i == id).then_some(()));
    h.wait(|e| matches!(e, Event::ServerExited { .. }).then_some(()));
    // et les demandes suivantes échouent proprement au lieu d'attendre indéfiniment
    let id = h.id();
    h.tx.send(Request::Hover {
        id,
        path: h.path.clone(),
        line: 0,
        col16: 0,
    })
    .unwrap();
    h.wait(|e| matches!(e, Event::Unavailable { id: i, .. } if i == id).then_some(()));
}

#[test]
fn un_serveur_lent_ne_bloque_pas_l_appelant() {
    let mut h = start(&["--delay-ms", "300"], "fake");
    h.open("fake", "abc\n");
    // l'envoi des requêtes est immédiat, même si le serveur met 300 ms à répondre
    let started = Instant::now();
    let ids: Vec<u64> = (0..3).map(|_| h.id()).collect();
    for &id in &ids {
        h.tx.send(Request::Hover {
            id,
            path: h.path.clone(),
            line: 0,
            col16: 0,
        })
        .unwrap();
    }
    assert!(started.elapsed() < Duration::from_millis(100));
    let mut seen = 0;
    h.wait(|e| {
        if matches!(e, Event::Hover { .. }) {
            seen += 1;
        }
        (seen == 3).then_some(())
    });
}

#[test]
fn fermeture_et_arret() {
    let h = start(&[], "fake");
    h.open("fake", "x\n");
    h.wait(|e| matches!(e, Event::ServerStarted { .. }).then_some(()));
    h.tx.send(Request::Save {
        path: h.path.clone(),
    })
    .unwrap();
    h.tx.send(Request::Close {
        path: h.path.clone(),
    })
    .unwrap();
    h.tx.send(Request::Shutdown).unwrap();
    // le thread s'arrête : le canal des demandes finit par être fermé
    let end = Instant::now() + TIMEOUT;
    while h
        .tx
        .send(Request::Save {
            path: h.path.clone(),
        })
        .is_ok()
    {
        assert!(Instant::now() < end, "le thread du client ne s'arrête pas");
        std::thread::sleep(Duration::from_millis(20));
    }
}
