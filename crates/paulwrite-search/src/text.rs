//! Recherche de texte dans les fichiers d'un projet (moteur de ripgrep).

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};

/// Longueur maximale (octets) du texte d'une ligne renvoyée : une ligne de plusieurs Mo
/// (fichier minifié) ne doit ni saturer le canal ni l'affichage.
const MAX_LINE_BYTES: usize = 240;
/// Lot de résultats envoyé dès qu'il atteint cette taille…
const BATCH: usize = 32;
/// … ou dès que cette durée s'est écoulée depuis le dernier envoi (premiers résultats rapides).
const BATCH_DELAY: Duration = Duration::from_millis(15);

/// Ce qu'on cherche.
#[derive(Clone, Debug)]
pub struct TextQuery {
    pub pattern: String,
    /// Expression régulière ; sinon texte littéral.
    pub regex: bool,
    pub case_sensitive: bool,
}

/// Réglages de la recherche.
#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Chercher aussi dans les fichiers et dossiers cachés.
    pub hidden: bool,
    /// Arrêt après ce nombre de résultats.
    pub max_results: usize,
    /// Fichiers plus gros ignorés (octets).
    pub max_filesize: u64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            hidden: false,
            max_results: 10_000,
            max_filesize: 5 * 1024 * 1024,
        }
    }
}

/// Une ligne qui correspond.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Chemin relatif à la racine de la recherche.
    pub path: String,
    /// Numéro de ligne (à partir de 1).
    pub line: u64,
    /// Texte de la ligne, sans fin de ligne, tronqué à 240 octets.
    pub text: String,
    /// Plages d'octets de `text` qui correspondent.
    pub ranges: Vec<(usize, usize)>,
}

/// Bilan d'une recherche terminée.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    /// Fichiers examinés.
    pub files: usize,
    /// Lignes trouvées.
    pub matches: usize,
    /// La recherche s'est arrêtée à `max_results`.
    pub truncated: bool,
    pub elapsed: Duration,
}

/// Messages envoyés par [`search_text`].
#[derive(Debug)]
pub enum Event {
    Matches(Vec<Match>),
    /// Toujours le dernier message (sauf si le récepteur a disparu).
    Done(Summary),
}

/// Lot de résultats vidé dans le canal quand il est plein, ancien, ou à la fin du thread.
struct Batcher {
    tx: Sender<Event>,
    buf: Vec<Match>,
    last: Instant,
    /// Le récepteur a disparu : inutile de continuer.
    closed: bool,
}

impl Batcher {
    fn push(&mut self, m: Match) {
        self.buf.push(m);
        if self.buf.len() >= BATCH || self.last.elapsed() >= BATCH_DELAY {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            if self
                .tx
                .send(Event::Matches(std::mem::take(&mut self.buf)))
                .is_err()
            {
                self.closed = true;
            }
            self.last = Instant::now();
        }
    }
}

impl Drop for Batcher {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Recueille les lignes qui correspondent dans un fichier.
struct FileSink<'a> {
    path: &'a Path,
    root: &'a Path,
    rel: Option<String>,
    matcher: &'a RegexMatcher,
    out: &'a mut Batcher,
    found: &'a AtomicUsize,
    max: usize,
    stop: &'a AtomicBool,
}

impl Sink for FileSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.found.fetch_add(1, Ordering::Relaxed) >= self.max {
            self.stop.store(true, Ordering::Relaxed);
            return Ok(false);
        }
        let mut bytes = mat.bytes();
        while let [rest @ .., b'\n' | b'\r'] = bytes {
            bytes = rest;
        }
        // Troncature sur une frontière de caractère (la ligne peut ne pas être de l'UTF-8 valide).
        let mut end = bytes.len().min(MAX_LINE_BYTES);
        while end < bytes.len() && end > 0 && (bytes[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
        let mut ranges = Vec::new();
        let _ = self.matcher.find_iter(text.as_bytes(), |m| {
            if m.end() > m.start() {
                ranges.push((m.start(), m.end()));
            }
            true
        });
        let (root, path) = (self.root, self.path);
        let rel = self.rel.get_or_insert_with(|| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        });
        self.out.push(Match {
            path: rel.clone(),
            line: mat.line_number().unwrap_or(0),
            text,
            ranges,
        });
        Ok(!self.out.closed)
    }
}

/// Cherche `query` dans les fichiers sous `root` (en respectant `.gitignore`, sans les fichiers
/// binaires), sur tous les cœurs. Les résultats arrivent par lots dans `tx`, suivis de
/// [`Event::Done`]. S'arrête dès que `cancel` passe à `true`.
///
/// # Errors
/// Motif invalide (le message décrit l'erreur de syntaxe).
pub fn search_text(
    root: &Path,
    query: &TextQuery,
    opts: &SearchOptions,
    cancel: &AtomicBool,
    tx: Sender<Event>,
) -> Result<(), String> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!query.case_sensitive)
        .fixed_strings(!query.regex)
        .build(&query.pattern)
        .map_err(|e| e.to_string())?;

    let started = Instant::now();
    let found = AtomicUsize::new(0);
    let files = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);

    WalkBuilder::new(root)
        .hidden(!opts.hidden)
        .require_git(false)
        .max_filesize(Some(opts.max_filesize))
        .build_parallel()
        .run(|| {
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(0))
                .line_number(true)
                .build();
            let mut out = Batcher {
                tx: tx.clone(),
                buf: Vec::with_capacity(BATCH),
                last: Instant::now(),
                closed: false,
            };
            let matcher = matcher.clone();
            let (found, files, stop) = (&found, &files, &stop);
            let max = opts.max_results;
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) || out.closed {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                files.fetch_add(1, Ordering::Relaxed);
                let mut sink = FileSink {
                    path: entry.path(),
                    root,
                    rel: None,
                    matcher: &matcher,
                    out: &mut out,
                    found,
                    max,
                    stop,
                };
                // Un fichier illisible (droits, verrouillé) est simplement ignoré.
                let _ = searcher.search_path(&matcher, entry.path(), &mut sink);
                WalkState::Continue
            })
        });

    let matches = found.load(Ordering::Relaxed).min(opts.max_results);
    let _ = tx.send(Event::Done(Summary {
        files: files.load(Ordering::Relaxed),
        matches,
        truncated: stop.load(Ordering::Relaxed),
        elapsed: started.elapsed(),
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk::testutil::make_project;
    use std::fs;

    fn run(
        root: &Path,
        pattern: &str,
        regex: bool,
        case: bool,
        max: usize,
    ) -> (Vec<Match>, Summary) {
        let (tx, rx) = crossbeam_channel::unbounded();
        search_text(
            root,
            &TextQuery {
                pattern: pattern.to_owned(),
                regex,
                case_sensitive: case,
            },
            &SearchOptions {
                max_results: max,
                ..SearchOptions::default()
            },
            &AtomicBool::new(false),
            tx,
        )
        .unwrap();
        let mut matches = Vec::new();
        let mut summary = None;
        for e in rx {
            match e {
                Event::Matches(m) => matches.extend(m),
                Event::Done(s) => summary = Some(s),
            }
        }
        matches.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        (matches, summary.expect("Done"))
    }

    #[test]
    fn texte_litteral_avec_positions() {
        let dir = make_project();
        let (m, s) = run(dir.path(), "needle", false, true, 100);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].path.replace('\\', "/"), "src/sub/lib.rs");
        assert_eq!(m[0].line, 1);
        assert_eq!(m[0].text, "pub fn needle() {}");
        assert_eq!(m[0].ranges, vec![(7, 13)]);
        assert!(!s.truncated);
        // les fichiers ignorés ne sont pas examinés : README, main.rs, lib.rs
        assert_eq!(s.files, 3);
    }

    #[test]
    fn casse_et_expressions_regulieres() {
        let dir = make_project();
        assert_eq!(run(dir.path(), "NEEDLE", false, true, 100).0.len(), 0);
        assert_eq!(run(dir.path(), "NEEDLE", false, false, 100).0.len(), 1);
        // regex : `fn \w+` trouve main et needle
        let (m, _) = run(dir.path(), r"fn \w+", true, true, 100);
        assert_eq!(m.len(), 2);
        // le même motif en littéral ne trouve rien
        assert_eq!(run(dir.path(), r"fn \w+", false, true, 100).0.len(), 0);
    }

    #[test]
    fn motif_invalide() {
        let dir = make_project();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let err = search_text(
            dir.path(),
            &TextQuery {
                pattern: "(".into(),
                regex: true,
                case_sensitive: true,
            },
            &SearchOptions::default(),
            &AtomicBool::new(false),
            tx,
        )
        .unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn fichiers_binaires_ignores_et_lignes_longues_tronquees() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bin.dat"), b"abc\0needle\0def").unwrap();
        let long = format!("{}needle{}\n", "é".repeat(400), "z".repeat(1000));
        fs::write(dir.path().join("long.txt"), long).unwrap();
        let (m, _) = run(dir.path(), "needle", false, true, 100);
        assert_eq!(m.len(), 1, "le binaire ne doit pas correspondre : {m:?}");
        assert_eq!(m[0].path, "long.txt");
        assert!(m[0].text.len() <= MAX_LINE_BYTES);
        assert!(
            m[0].text.chars().all(|c| c == 'é' || c == '\u{FFFD}'),
            "coupure en plein caractère"
        );
    }

    #[test]
    fn limite_de_resultats_et_annulation() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            fs::write(dir.path().join(format!("f{i}.txt")), "x\nx\nx\n").unwrap();
        }
        let (m, s) = run(dir.path(), "x", false, true, 20);
        assert_eq!(m.len(), 20);
        assert!(s.truncated);

        let (tx, rx) = crossbeam_channel::unbounded();
        search_text(
            dir.path(),
            &TextQuery {
                pattern: "x".into(),
                regex: false,
                case_sensitive: true,
            },
            &SearchOptions::default(),
            &AtomicBool::new(true),
            tx,
        )
        .unwrap();
        let events: Vec<Event> = rx.iter().collect();
        assert!(matches!(events.last(), Some(Event::Done(s)) if s.matches == 0));
    }
}
