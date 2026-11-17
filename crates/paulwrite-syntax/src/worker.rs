//! Thread dédié à la coloration : le thread UI envoie des [`Request`] et reçoit des [`Response`].
//!
//! Les requêtes en attente sont **regroupées** : si plusieurs frappes arrivent pendant une analyse,
//! on n'analyse qu'une fois, avec le dernier état du texte (les `TextEdit` s'accumulent pour que
//! l'arbre reste cohérent).

use std::collections::HashMap;
use std::ops::Range;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender, TryRecvError};
use paulwrite_core::TextEdit;
use ropey::Rope;

use crate::highlight::{Highlighter, Span};
use crate::Language;

/// Taille de la fenêtre analysée à l'ouverture, avant la première requête de fenêtre (octets).
const DEFAULT_WINDOW: usize = 32 * 1024;

/// Taille d'un morceau de la coloration du document entier (minimap) : entre deux morceaux, le
/// worker regarde s'il y a une requête plus urgente (frappe, défilement) et la traite d'abord.
const OVERVIEW_CHUNK: usize = 64 * 1024;

/// Demande envoyée au worker. `tab` identifie le document côté application.
pub enum Request {
    /// Nouveau document (ou document remplacé) : analyse complète.
    Open {
        tab: u64,
        language: Language,
        rope: Rope,
        revision: u64,
    },
    /// Le texte a changé : `rope` est le nouvel état (copie O(1)), `edits` les modifications
    /// depuis la dernière requête, dans l'ordre, `viewport` la fenêtre à colorer **dans le
    /// nouveau texte** (les décalages d'octets ont pu changer).
    Edit {
        tab: u64,
        rope: Rope,
        revision: u64,
        edits: Vec<TextEdit>,
        viewport: Range<usize>,
    },
    /// La fenêtre visible a changé : colorer cette plage d'octets.
    Viewport { tab: u64, range: Range<usize> },
    /// Colorer le **document entier** (minimap), par morceaux et seulement quand rien d'autre
    /// n'attend. Une modification du texte abandonne le calcul en cours : à redemander une fois
    /// la frappe terminée. La réponse a [`Response::overview`] à `true`.
    Overview { tab: u64 },
    /// Le document est fermé.
    Close { tab: u64 },
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Request::Open { tab, language, .. } => write!(f, "Open({tab}, {language:?})"),
            Request::Edit { tab, edits, .. } => write!(f, "Edit({tab}, {} modif.)", edits.len()),
            Request::Viewport { tab, range } => write!(f, "Viewport({tab}, {range:?})"),
            Request::Overview { tab } => write!(f, "Overview({tab})"),
            Request::Close { tab } => write!(f, "Close({tab})"),
        }
    }
}

/// Résultat envoyé au thread UI.
#[derive(Debug)]
pub struct Response {
    pub tab: u64,
    /// Révision du texte analysé : à comparer à celle du document avant d'afficher.
    pub revision: u64,
    /// Plage d'octets couverte (les octets de cette plage absents de `spans` sont du texte
    /// ordinaire).
    pub range: Range<usize>,
    pub spans: Vec<Span>,
    /// Durée de l'analyse + de la requête de coloration (hors file d'attente).
    pub elapsed: Duration,
    /// Coloration du document entier (réponse à [`Request::Overview`]) et non de la fenêtre.
    pub overview: bool,
}

/// Coloration du document entier en cours, morceau par morceau.
struct OverviewJob {
    revision: u64,
    next: usize,
    spans: Vec<Span>,
    started: Instant,
}

struct TabState {
    highlighter: Highlighter,
    rope: Rope,
    revision: u64,
    viewport: Range<usize>,
    pending_edits: Vec<TextEdit>,
    tree_dirty: bool,
    /// Le thread UI attend la coloration du document entier.
    overview_wanted: bool,
    overview: Option<OverviewJob>,
}

/// Démarre le worker. `notify` est appelé **depuis le worker** pour chaque réponse.
pub fn spawn(notify: impl Fn(Response) + Send + 'static) -> Sender<Request> {
    let (tx, rx) = unbounded::<Request>();
    thread::Builder::new()
        .name("paulwrite-syntax".into())
        .spawn(move || {
            let mut tabs: HashMap<u64, TabState> = HashMap::new();
            loop {
                // S'il y a une coloration entière à faire, on ne bloque pas : quand la file est
                // vide, on en fait un morceau, puis on regarde de nouveau la file.
                let first = if tabs.values().any(|t| t.overview_wanted) {
                    match rx.try_recv() {
                        Ok(r) => r,
                        Err(TryRecvError::Empty) => {
                            overview_step(&mut tabs, &notify);
                            continue;
                        }
                        Err(TryRecvError::Disconnected) => break,
                    }
                } else {
                    match rx.recv() {
                        Ok(r) => r,
                        Err(_) => break,
                    }
                };
                let mut batch = vec![first];
                while let Ok(r) = rx.try_recv() {
                    batch.push(r);
                }
                let mut touched: Vec<u64> = Vec::new();
                let touch = |t: u64, touched: &mut Vec<u64>| {
                    if !touched.contains(&t) {
                        touched.push(t);
                    }
                };
                for req in batch {
                    match req {
                        Request::Open {
                            tab,
                            language,
                            rope,
                            revision,
                        } => match Highlighter::new(language) {
                            Ok(highlighter) => {
                                let viewport = 0..rope.len_bytes().min(DEFAULT_WINDOW);
                                tabs.insert(
                                    tab,
                                    TabState {
                                        highlighter,
                                        rope,
                                        revision,
                                        viewport,
                                        pending_edits: Vec::new(),
                                        tree_dirty: true,
                                        overview_wanted: false,
                                        overview: None,
                                    },
                                );
                                touch(tab, &mut touched);
                            }
                            Err(e) => eprintln!("coloration indisponible ({language:?}) : {e}"),
                        },
                        Request::Edit {
                            tab,
                            rope,
                            revision,
                            edits,
                            viewport,
                        } => {
                            if let Some(st) = tabs.get_mut(&tab) {
                                st.viewport = viewport;
                                st.rope = rope;
                                st.revision = revision;
                                st.pending_edits.extend(edits);
                                st.tree_dirty = true;
                                // le texte a changé : la coloration entière en cours est périmée
                                st.overview_wanted = false;
                                st.overview = None;
                                touch(tab, &mut touched);
                            }
                        }
                        Request::Viewport { tab, range } => {
                            if let Some(st) = tabs.get_mut(&tab) {
                                st.viewport = range;
                                touch(tab, &mut touched);
                            }
                        }
                        Request::Overview { tab } => {
                            if let Some(st) = tabs.get_mut(&tab) {
                                st.overview_wanted = true;
                                st.overview = None;
                            }
                        }
                        Request::Close { tab } => {
                            tabs.remove(&tab);
                            touched.retain(|t| *t != tab);
                        }
                    }
                }
                for tab in touched {
                    let Some(st) = tabs.get_mut(&tab) else {
                        continue;
                    };
                    let started = Instant::now();
                    if st.tree_dirty {
                        let edits = std::mem::take(&mut st.pending_edits);
                        st.highlighter.update(&st.rope, &edits);
                        st.tree_dirty = false;
                    }
                    let range = st.viewport.start.min(st.rope.len_bytes())
                        ..st.viewport.end.min(st.rope.len_bytes());
                    let spans = st.highlighter.highlights(&st.rope, range.clone());
                    notify(Response {
                        tab,
                        revision: st.revision,
                        range,
                        spans,
                        elapsed: started.elapsed(),
                        overview: false,
                    });
                }
            }
        })
        .expect("thread de coloration");
    tx
}

/// Colore un morceau du document entier d'un onglet qui l'attend ; envoie la réponse quand le
/// document est terminé.
fn overview_step(tabs: &mut HashMap<u64, TabState>, notify: &impl Fn(Response)) {
    let Some((&tab, st)) = tabs.iter_mut().find(|(_, t)| t.overview_wanted) else {
        return;
    };
    if st.tree_dirty {
        let edits = std::mem::take(&mut st.pending_edits);
        st.highlighter.update(&st.rope, &edits);
        st.tree_dirty = false;
    }
    let len = st.rope.len_bytes();
    let (revision, rope) = (st.revision, &st.rope);
    let job = st.overview.get_or_insert_with(|| OverviewJob {
        revision,
        next: 0,
        spans: Vec::new(),
        started: Instant::now(),
    });
    // le morceau se termine sur un début de ligne
    let target = (job.next + OVERVIEW_CHUNK).min(len);
    let end = if target >= len {
        len
    } else {
        let line = rope.byte_to_line(target);
        if line + 1 < rope.len_lines() {
            rope.line_to_byte(line + 1)
        } else {
            len
        }
    };
    if end > job.next {
        job.spans
            .extend(st.highlighter.highlights(rope, job.next..end));
        job.next = end;
    }
    if job.next >= len {
        let done = st.overview.take().expect("calcul en cours");
        st.overview_wanted = false;
        notify(Response {
            tab,
            revision: done.revision,
            range: 0..len,
            spans: done.spans,
            elapsed: done.started.elapsed(),
            overview: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paulwrite_core::Buffer;
    use std::sync::mpsc;
    use std::sync::Mutex;

    #[test]
    fn ouverture_modification_et_fenetre() {
        let (out_tx, out_rx) = mpsc::channel();
        let out_tx = Mutex::new(out_tx);
        let tx = spawn(move |r| {
            let _ = out_tx.lock().unwrap().send(r);
        });
        let wait =
            |rx: &mpsc::Receiver<Response>| rx.recv_timeout(Duration::from_secs(20)).unwrap();

        let mut buf = Buffer::from_text("fn main() {}\n");
        buf.track_edits(true);
        tx.send(Request::Open {
            tab: 7,
            language: Language::Rust,
            rope: buf.rope().clone(),
            revision: buf.revision(),
        })
        .unwrap();
        let r = wait(&out_rx);
        assert_eq!((r.tab, r.revision), (7, buf.revision()));
        assert!(r.spans.iter().any(|s| s.kind == crate::kind::KEYWORD));

        // une modification : l'arbre est mis à jour de façon incrémentale
        buf.insert(0, "// c\n");
        tx.send(Request::Edit {
            tab: 7,
            rope: buf.rope().clone(),
            revision: buf.revision(),
            edits: buf.take_edits(),
            viewport: 0..buf.len_bytes(),
        })
        .unwrap();
        let r = wait(&out_rx);
        assert_eq!(r.revision, buf.revision());
        assert!(r
            .spans
            .iter()
            .any(|s| s.kind == crate::kind::COMMENT && s.start == 0));

        // fenêtre plus petite
        tx.send(Request::Viewport {
            tab: 7,
            range: 6..buf.len_bytes(),
        })
        .unwrap();
        let r = wait(&out_rx);
        assert_eq!(r.range.start, 6);
        assert!(r.spans.iter().all(|s| s.start >= 6));

        // onglet inconnu ou fermé : pas de réponse, pas de panique
        tx.send(Request::Close { tab: 7 }).unwrap();
        tx.send(Request::Viewport {
            tab: 7,
            range: 0..1,
        })
        .unwrap();
        assert!(out_rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn coloration_du_document_entier_par_morceaux() {
        let (out_tx, out_rx) = mpsc::channel();
        let out_tx = Mutex::new(out_tx);
        let tx = spawn(move |r| {
            let _ = out_tx.lock().unwrap().send(r);
        });
        // ~200 Ko : plusieurs morceaux
        let mut text = String::new();
        for i in 0..4000 {
            text.push_str(&format!("fn f{i}() {{ let x = {i}; }} // fin\n"));
        }
        let mut buf = Buffer::from_text(&text);
        buf.track_edits(true);
        tx.send(Request::Open {
            tab: 1,
            language: Language::Rust,
            rope: buf.rope().clone(),
            revision: buf.revision(),
        })
        .unwrap();
        let first = out_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(!first.overview, "la fenêtre d'abord");

        tx.send(Request::Overview { tab: 1 }).unwrap();
        let r = loop {
            let r = out_rx.recv_timeout(Duration::from_secs(20)).unwrap();
            if r.overview {
                break r;
            }
        };
        assert_eq!(r.range, 0..buf.len_bytes());
        assert_eq!(r.revision, buf.revision());
        // le document entier est couvert : des mots-clés au début et à la fin, triés
        assert!(r
            .spans
            .iter()
            .any(|s| s.kind == crate::kind::KEYWORD && s.start < 100));
        assert!(r
            .spans
            .iter()
            .any(|s| s.kind == crate::kind::KEYWORD && s.start > buf.len_bytes() - 100));
        assert!(r.spans.windows(2).all(|w| w[0].end <= w[1].start));

        // une modification pendant le calcul l'abandonne : pas de réponse périmée
        tx.send(Request::Overview { tab: 1 }).unwrap();
        buf.insert(0, "// x\n");
        tx.send(Request::Edit {
            tab: 1,
            rope: buf.rope().clone(),
            revision: buf.revision(),
            edits: buf.take_edits(),
            viewport: 0..100,
        })
        .unwrap();
        let mut stale = false;
        while let Ok(r) = out_rx.recv_timeout(Duration::from_millis(800)) {
            if r.overview && r.revision != buf.revision() {
                stale = true;
            }
        }
        assert!(!stale, "aucune coloration entière d'un texte dépassé");
    }
}
