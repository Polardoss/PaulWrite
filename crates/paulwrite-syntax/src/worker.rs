//! Thread dédié à la coloration : le thread UI envoie des [`Request`] et reçoit des [`Response`].
//!
//! Les requêtes en attente sont **regroupées** : si plusieurs frappes arrivent pendant une analyse,
//! on n'analyse qu'une fois, avec le dernier état du texte (les `TextEdit` s'accumulent pour que
//! l'arbre reste cohérent).

use std::collections::HashMap;
use std::ops::Range;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};
use paulwrite_core::TextEdit;
use ropey::Rope;

use crate::highlight::{Highlighter, Span};
use crate::Language;

/// Taille de la fenêtre analysée à l'ouverture, avant la première requête de fenêtre (octets).
const DEFAULT_WINDOW: usize = 32 * 1024;

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
    /// Le document est fermé.
    Close { tab: u64 },
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Request::Open { tab, language, .. } => write!(f, "Open({tab}, {language:?})"),
            Request::Edit { tab, edits, .. } => write!(f, "Edit({tab}, {} modif.)", edits.len()),
            Request::Viewport { tab, range } => write!(f, "Viewport({tab}, {range:?})"),
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
}

struct TabState {
    highlighter: Highlighter,
    rope: Rope,
    revision: u64,
    viewport: Range<usize>,
    pending_edits: Vec<TextEdit>,
    tree_dirty: bool,
}

/// Démarre le worker. `notify` est appelé **depuis le worker** pour chaque réponse.
pub fn spawn(notify: impl Fn(Response) + Send + 'static) -> Sender<Request> {
    let (tx, rx) = unbounded::<Request>();
    thread::Builder::new()
        .name("paulwrite-syntax".into())
        .spawn(move || {
            let mut tabs: HashMap<u64, TabState> = HashMap::new();
            while let Ok(first) = rx.recv() {
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
                                touch(tab, &mut touched);
                            }
                        }
                        Request::Viewport { tab, range } => {
                            if let Some(st) = tabs.get_mut(&tab) {
                                st.viewport = range;
                                touch(tab, &mut touched);
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
                    });
                }
            }
        })
        .expect("thread de coloration");
    tx
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
}
