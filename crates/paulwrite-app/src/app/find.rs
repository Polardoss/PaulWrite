//! Rechercher et remplacer dans le document courant : Ctrl+F (chercher), Ctrl+H (remplacer),
//! F3 / Maj+F3 (occurrence suivante / précédente).
//!
//! Le panneau est celui de la palette (saisie en haut) ; en mode remplacement, une seconde saisie
//! s'y ajoute (Tab pour passer de l'une à l'autre). La recherche se fait **sur un thread**, dans
//! une copie O(1) du texte (`Rope`) : le thread de l'interface ne parcourt jamais le document. Les
//! occurrences sont surlignées dans le texte, la première à partir du curseur est sélectionnée au
//! fil de la frappe.

use paulwrite_search::{find_in_rope, Found, Matches};

use super::*;

/// Nombre maximal d'occurrences conservées (surbrillance comprise).
pub(super) const FIND_LIMIT: usize = 20_000;

/// Où aller quand les résultats arrivent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Jump {
    /// La première occurrence à partir de cette position (en bouclant).
    From(usize),
    /// L'occurrence suivante (`true`) ou précédente.
    Step(bool),
}

/// Résultat d'une recherche, envoyé par son thread.
#[derive(Debug)]
pub struct FoundEvent {
    generation: u64,
    revision: u64,
    result: Result<Matches, String>,
}

/// Recherche en cours ou terminée.
pub(super) struct FindState {
    tab: TabId,
    generation: u64,
    cancel: Arc<AtomicBool>,
    /// Révision du texte sur lequel porte la recherche.
    revision: u64,
    matches: Vec<Found>,
    truncated: bool,
    error: Option<String>,
    pending: Option<Jump>,
    /// Les résultats sont arrivés.
    done: bool,
    /// Le panneau est ouvert (surbrillance et message) ; sinon c'est un F3.
    panel: bool,
}

/// Indice de l'occurrence où aller : depuis une position (la première à partir d'elle, en
/// bouclant) ou depuis la sélection (la suivante ou la précédente, en bouclant).
pub(super) fn jump_index(
    matches: &[Found],
    jump: Jump,
    selection: (usize, usize),
) -> Option<usize> {
    let n = matches.len();
    if n == 0 {
        return None;
    }
    let (start, end) = selection;
    Some(match jump {
        Jump::From(p) => matches.partition_point(|m| m.start < p) % n,
        Jump::Step(true) => matches.partition_point(|m| m.start < end) % n,
        Jump::Step(false) => match matches.partition_point(|m| m.end <= start) {
            0 => n - 1,
            k => k - 1,
        },
    })
}

impl App {
    /// Le panneau de recherche (ou de remplacement) est ouvert.
    pub(super) fn find_panel_open(&self) -> bool {
        self.palette
            .as_ref()
            .is_some_and(|s| matches!(s.palette.mode(), PaletteMode::Find | PaletteMode::Replace))
    }

    /// Prépare le panneau qui vient de s'ouvrir : saisie préremplie (texte sélectionné sur une
    /// ligne, sinon la dernière recherche), second champ en mode remplacement.
    pub(super) fn find_prepare(&mut self, st: &mut PaletteState) {
        let sel = ed!(self).selection_string(200);
        let prefill = if !sel.is_empty() && !sel.contains(['\n', '\r']) {
            sel
        } else {
            self.last_find.clone()
        };
        if !prefill.is_empty() {
            st.palette.handle(&PaletteKey::Text(prefill));
        }
        if st.palette.mode() == PaletteMode::Replace {
            st.replace = Some(self.new_palette(PaletteMode::ReplaceWith));
            st.focus_replace = false;
        }
        self.find_origin = ed!(self).selection_range().0;
    }

    /// Le panneau vient de s'ouvrir : lance la recherche de ce qui est déjà saisi.
    pub(super) fn find_opened(&mut self) {
        let has_query = self
            .palette
            .as_ref()
            .is_some_and(|s| !s.palette.query().is_empty());
        if has_query {
            self.start_find(Some(Jump::From(self.find_origin)));
        }
        self.find_refresh_note();
    }

    /// Le panneau se ferme : on retient la recherche (pour F3), on retire la surbrillance.
    pub(super) fn find_closed(&mut self, query: &str) {
        if !query.is_empty() {
            self.last_find = query.to_owned();
        }
        if let Some(f) = self.find.take() {
            f.cancel.store(true, Ordering::Relaxed);
        }
        ed!(self).clear_matches();
    }

    /// (Re)lance la recherche. Sans panneau : celle de `last_find`, pour F3.
    pub(super) fn start_find(&mut self, pending: Option<Jump>) {
        let (query, replacement, panel) = match self.palette.as_ref() {
            Some(st) if self.find_panel_open() => (
                st.palette.query().to_owned(),
                st.replace.as_ref().map(|r| r.query().to_owned()),
                true,
            ),
            _ => (self.last_find.clone(), None, false),
        };
        if let Some(f) = self.find.take() {
            f.cancel.store(true, Ordering::Relaxed);
        }
        ed!(self).clear_matches();
        if query.is_empty() {
            self.find_refresh_note();
            return;
        }
        self.find_generation += 1;
        let generation = self.find_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let tab = &self.tabs[self.active];
        let (rope, revision, tab_id) = (
            tab.editor.document().buffer().rope().clone(),
            tab.editor.revision(),
            tab.id,
        );
        self.find = Some(FindState {
            tab: tab_id,
            generation,
            cancel: cancel.clone(),
            revision,
            matches: Vec::new(),
            truncated: false,
            error: None,
            pending,
            done: false,
            panel,
        });
        let (opts, proxy) = (self.find_opts, self.proxy.clone());
        std::thread::spawn(move || {
            let result = find_in_rope(
                &rope,
                &query,
                opts,
                replacement.as_deref(),
                FIND_LIMIT,
                &cancel,
            );
            let _ = proxy.send_event(UserEvent::Found(FoundEvent {
                generation,
                revision,
                result,
            }));
        });
        self.find_refresh_note();
    }

    /// Les résultats d'une recherche arrivent.
    pub(super) fn on_found(&mut self, ev: FoundEvent) {
        let Some(f) = self.find.as_mut() else {
            return;
        };
        if ev.generation != f.generation {
            return; // une recherche plus récente est en route
        }
        let Some(index) = self.tabs.iter().position(|t| t.id == f.tab) else {
            self.find = None;
            return;
        };
        match ev.result {
            Err(e) => {
                f.error = Some(e.lines().next().unwrap_or_default().to_owned());
                f.matches.clear();
                f.done = true;
            }
            Ok(m) if m.cancelled => return,
            Ok(m) => {
                // le texte a changé depuis le départ de la recherche : on la refait
                if ev.revision != self.tabs[index].editor.revision() {
                    let pending = f.pending;
                    self.start_find(pending);
                    return;
                }
                f.matches = m.found;
                f.truncated = m.truncated;
                f.error = None;
                f.done = true;
                if f.panel && index == self.active {
                    let ranges = f.matches.iter().map(|m| (m.start, m.end)).collect();
                    self.tabs[index].editor.set_matches(ev.revision, ranges);
                }
            }
        }
        if let Some(jump) = self.find.as_mut().and_then(|f| f.pending.take()) {
            self.find_jump(jump);
        }
        self.find_refresh_note();
        self.request_redraw();
    }

    /// Va à l'occurrence désignée par `jump` (parmi celles qui sont connues).
    fn find_jump(&mut self, jump: Jump) {
        let Some(f) = self.find.as_ref() else {
            return;
        };
        let n = f.matches.len();
        if n == 0 {
            if !f.panel && f.error.is_none() {
                self.toast(self.lang.pick("No matches", "Aucune occurrence"));
            }
            return;
        }
        let selection = ed!(self).selection_range();
        let Some(at) = jump_index(&f.matches, jump, selection) else {
            return;
        };
        let m = &f.matches[at];
        let (a, b) = (m.start, m.end);
        ed!(self).select_range(a, b);
        self.touch(Instant::now());
    }

    /// Occurrence suivante ou précédente, avec le panneau (Entrée) ou sans (F3).
    pub(super) fn find_step(&mut self, forward: bool) {
        let ready = {
            let ed = &self.tabs[self.active];
            self.find
                .as_ref()
                .is_some_and(|f| f.done && f.tab == ed.id && f.revision == ed.editor.revision())
        };
        if self.find_panel_open() && ready {
            self.find_jump(Jump::Step(forward));
            self.find_refresh_note();
            self.request_redraw();
        } else if self.find_panel_open() || !self.last_find.is_empty() {
            self.start_find(Some(Jump::Step(forward)));
        } else {
            self.toast(
                self.lang
                    .pick("Nothing to find yet", "Rien à chercher pour l'instant"),
            );
        }
    }

    /// Touche reçue par le panneau de recherche ou de remplacement.
    pub(super) fn find_key(&mut self, key: &PaletteKey) {
        let shift = self.mods.shift_key();
        let (ctrl, alt) = (self.mods.control_key(), self.mods.alt_key());
        let Some(st) = self.palette.as_mut() else {
            return;
        };
        match key {
            PaletteKey::Tab => {
                if st.replace.is_some() {
                    st.focus_replace = !st.focus_replace;
                    self.overlay_dirty = true;
                }
            }
            PaletteKey::Escape => self.close_palette(),
            PaletteKey::Enter if ctrl && alt => self.replace_all(),
            PaletteKey::Enter if st.focus_replace => self.replace_current(),
            PaletteKey::Enter => self.find_step(!shift),
            other => {
                let in_replace = st.focus_replace;
                let target = match (in_replace, st.replace.as_mut()) {
                    (true, Some(r)) => r,
                    _ => &mut st.palette,
                };
                match target.handle(other) {
                    Outcome::QueryChanged if in_replace => {
                        // le texte de remplacement change : recalculer pour les groupes `$1`
                        self.start_find(Some(Jump::From(self.find_origin)));
                    }
                    Outcome::QueryChanged => self.start_find(Some(Jump::From(self.find_origin))),
                    Outcome::Redraw => self.overlay_dirty = true,
                    Outcome::None | Outcome::Accept | Outcome::Close => {}
                }
            }
        }
        self.overlay_dirty = true;
    }

    /// Alt+C (casse), Alt+W (mot entier), Alt+R (expression régulière).
    pub(super) fn find_toggle(&mut self, letter: char) {
        match letter {
            'c' => self.find_opts.case_sensitive = !self.find_opts.case_sensitive,
            'w' => self.find_opts.whole_word = !self.find_opts.whole_word,
            'r' => self.find_opts.regex = !self.find_opts.regex,
            _ => return,
        }
        self.start_find(Some(Jump::From(self.find_origin)));
    }

    /// Remplace l'occurrence sélectionnée puis passe à la suivante ; si la sélection n'est pas
    /// une occurrence, va simplement à la prochaine.
    fn replace_current(&mut self) {
        let (start, end) = ed!(self).selection_range();
        let ready = {
            let ed = &self.tabs[self.active];
            self.find
                .as_ref()
                .is_some_and(|f| f.done && f.tab == ed.id && f.revision == ed.editor.revision())
        };
        let found = self.find.as_ref().filter(|_| ready).and_then(|f| {
            f.matches
                .binary_search_by(|m| (m.start, m.end).cmp(&(start, end)))
                .ok()
                .map(|i| f.matches[i].replacement.clone().unwrap_or_default())
        });
        let Some(text) = found else {
            self.find_step(true);
            return;
        };
        let after = start + text.chars().count();
        let now = self.now_ms();
        ed!(self).replace_ranges(&[(start, end, text)], now);
        // la recherche est refaite sur le nouveau texte, puis on va à l'occurrence suivante
        self.start_find(Some(Jump::From(after)));
        self.request_redraw();
    }

    /// Remplace toutes les occurrences, en un seul pas d'annulation.
    fn replace_all(&mut self) {
        let ready = {
            let ed = &self.tabs[self.active];
            self.find
                .as_ref()
                .is_some_and(|f| f.done && f.tab == ed.id && f.revision == ed.editor.revision())
        };
        let Some(f) = self.find.as_ref().filter(|_| ready) else {
            self.toast(self.lang.pick(
                "Search in progress, try again",
                "Recherche en cours, réessayez",
            ));
            return;
        };
        let repl: Vec<paulwrite_core::Replacement> = f
            .matches
            .iter()
            .map(|m| (m.start, m.end, m.replacement.clone().unwrap_or_default()))
            .collect();
        if repl.is_empty() {
            self.toast(self.lang.pick("No matches", "Aucune occurrence"));
            return;
        }
        let (n, truncated) = (repl.len(), f.truncated);
        let now = self.now_ms();
        ed!(self).replace_ranges(&repl, now);
        self.toast(match (self.lang, truncated) {
            (Lang::En, false) => format!("Replaced {n} occurrences"),
            (Lang::Fr, false) => format!("{n} occurrences remplacées"),
            (Lang::En, true) => format!("Replaced {n} occurrences (limit reached: repeat)"),
            (Lang::Fr, true) => {
                format!("{n} occurrences remplacées (limite atteinte : recommencez)")
            }
        });
        self.start_find(None);
        self.request_redraw();
    }

    /// Texte sous la saisie : nombre d'occurrences, position de la sélection, options actives.
    fn find_note(&self) -> String {
        let Some(st) = self.palette.as_ref() else {
            return String::new();
        };
        if st.palette.query().is_empty() {
            return String::new();
        }
        let lang = self.lang;
        let mut note = match self.find.as_ref() {
            None => String::new(),
            Some(f) if !f.done => lang
                .pick("Searching\u{2026}", "Recherche\u{2026}")
                .to_owned(),
            Some(f) => match &f.error {
                Some(e) => e.clone(),
                None if f.matches.is_empty() => {
                    lang.pick("No matches", "Aucune occurrence").to_owned()
                }
                None => {
                    let n = f.matches.len();
                    let plus = if f.truncated { "+" } else { "" };
                    let (s, e) = self.tabs[self.active].editor.selection_range();
                    match f
                        .matches
                        .binary_search_by(|m| (m.start, m.end).cmp(&(s, e)))
                    {
                        Ok(i) => lang.pick(
                            format!("{} of {n}{plus}", i + 1),
                            format!("{} sur {n}{plus}", i + 1),
                        ),
                        Err(_) => lang.pick(
                            format!("{n}{plus} matches"),
                            format!("{n}{plus} occurrences"),
                        ),
                    }
                }
            },
        };
        let o = self.find_opts;
        for (on, label) in [
            (o.case_sensitive, lang.pick("[case]", "[casse]")),
            (o.whole_word, lang.pick("[word]", "[mot]")),
            (o.regex, "[regex]"),
        ] {
            if on {
                note.push_str("  ");
                note.push_str(label);
            }
        }
        note
    }

    /// Met à jour le message du panneau.
    pub(super) fn find_refresh_note(&mut self) {
        if !self.find_panel_open() {
            return;
        }
        let note = self.find_note();
        if let Some(st) = self.palette.as_mut() {
            st.palette.set_note(note);
        }
        self.overlay_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(ranges: &[(usize, usize)]) -> Vec<Found> {
        ranges
            .iter()
            .map(|&(start, end)| Found {
                start,
                end,
                replacement: None,
            })
            .collect()
    }

    #[test]
    fn aller_a_une_occurrence() {
        let m = found(&[(2, 4), (10, 12), (20, 22)]);
        // à partir d'une position : la première qui commence à cette position ou après, en bouclant
        assert_eq!(jump_index(&m, Jump::From(0), (0, 0)), Some(0));
        assert_eq!(jump_index(&m, Jump::From(3), (0, 0)), Some(1));
        assert_eq!(jump_index(&m, Jump::From(10), (0, 0)), Some(1));
        assert_eq!(
            jump_index(&m, Jump::From(21), (0, 0)),
            Some(0),
            "au-delà de la dernière : on reboucle"
        );
        // suivante : après la sélection ; sur la dernière, on revient à la première
        assert_eq!(jump_index(&m, Jump::Step(true), (2, 4)), Some(1));
        assert_eq!(jump_index(&m, Jump::Step(true), (10, 12)), Some(2));
        assert_eq!(jump_index(&m, Jump::Step(true), (20, 22)), Some(0));
        assert_eq!(
            jump_index(&m, Jump::Step(true), (5, 5)),
            Some(1),
            "curseur entre deux occurrences"
        );
        // précédente : avant la sélection ; sur la première, on va à la dernière
        assert_eq!(jump_index(&m, Jump::Step(false), (20, 22)), Some(1));
        assert_eq!(jump_index(&m, Jump::Step(false), (2, 4)), Some(2));
        assert_eq!(jump_index(&m, Jump::Step(false), (15, 15)), Some(1));
        assert_eq!(jump_index(&m, Jump::Step(false), (0, 0)), Some(2));
        // rien à trouver
        assert_eq!(jump_index(&[], Jump::Step(true), (0, 0)), None);
    }
}
