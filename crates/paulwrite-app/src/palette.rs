//! État du panneau flottant (Goto Anything, palette de commandes, aller à la ligne, recherche dans
//! le projet) : saisie, liste de résultats, sélection et défilement. Aucune dépendance à winit :
//! entièrement testable. Les résultats viennent de l'application (fuzzy finder, recherche), le
//! rendu du moteur graphique ([`Palette::to_overlay`]).

use paulwrite_render::{Overlay, OverlayRow, OverlayRowKind};

use crate::command::Command;

/// Nombre de résultats visibles à la fois.
pub const MAX_VISIBLE: usize = 10;

/// Ce que le panneau cherche.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    /// Goto Anything : un fichier du projet.
    Files,
    /// Palette de commandes.
    Commands,
    /// Aller à la ligne.
    GotoLine,
    /// Recherche de texte dans le projet.
    Search,
    /// Complétion proposée par un serveur de langage (la saisie filtre la liste).
    Completion,
    /// Liste des diagnostics du document courant (lecture seule).
    Diagnostics,
    /// Emplacements d'une définition (lecture seule).
    Locations,
    /// Texte de survol d'un serveur de langage (lecture seule).
    Hover,
}

impl PaletteMode {
    /// La saisie n'a pas de sens : la liste est fixe (diagnostics, définitions, survol).
    #[must_use]
    pub fn is_static(self) -> bool {
        matches!(
            self,
            PaletteMode::Diagnostics | PaletteMode::Locations | PaletteMode::Hover
        )
    }
}

impl PaletteMode {
    /// Invite affichée devant la saisie.
    fn prompt(self) -> &'static str {
        match self {
            PaletteMode::Files => "",
            PaletteMode::Commands => "> ",
            PaletteMode::GotoLine => ": ",
            PaletteMode::Search => "? ",
            PaletteMode::Completion => "",
            PaletteMode::Diagnostics => "Diagnostics",
            PaletteMode::Locations => "Définitions",
            PaletteMode::Hover => "Survol",
        }
    }
}

/// Ce que fait la validation d'un résultat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Ouvrir ce fichier (chemin relatif au projet).
    File(String),
    /// Exécuter cette commande.
    Command(Command),
    /// Aller à cette ligne (à partir de 1) du document courant.
    Line(usize),
    /// Ouvrir ce fichier à cette ligne.
    Match { path: String, line: u64 },
    /// Exécuter cette commande de plugin (par son nom).
    Plugin(String),
    /// Insérer ce texte à la place des `usize` caractères d'identifiant qui précèdent le curseur
    /// (complétion).
    Completion(String, usize),
    /// Rien à faire (ligne d'information, par exemple le texte d'un survol).
    Nothing,
}

/// Un résultat de la liste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// Texte secondaire aligné à droite (raccourci, numéro de ligne…).
    pub detail: String,
    /// Indices (en caractères) de `label` à mettre en évidence.
    pub highlights: Vec<u32>,
    pub target: Target,
}

/// Touche transmise au panneau.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteKey {
    /// Texte saisi (frappe ou validation d'un IME).
    Text(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    PageUp,
    PageDown,
    Enter,
    Escape,
}

/// Conséquence d'une touche, pour l'application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Rien à faire.
    None,
    /// Il faut redessiner (curseur ou sélection déplacés).
    Redraw,
    /// La saisie a changé : recalculer les résultats.
    QueryChanged,
    /// Valider le résultat sélectionné.
    Accept,
    /// Fermer le panneau.
    Close,
}

#[derive(Debug)]
pub struct Palette {
    mode: PaletteMode,
    query: String,
    /// Position du curseur dans la saisie, en caractères.
    caret: usize,
    items: Vec<Item>,
    /// Pour chaque résultat : sa mise en évidence est calculée (voir `fill_visible_highlights`).
    filled: Vec<bool>,
    selected: usize,
    /// Premier résultat visible.
    offset: usize,
    /// Message affiché sous la liste (progression, nombre de résultats).
    note: String,
}

impl Palette {
    #[must_use]
    pub fn new(mode: PaletteMode) -> Self {
        Self {
            mode,
            query: String::new(),
            caret: 0,
            items: Vec::new(),
            filled: Vec::new(),
            selected: 0,
            offset: 0,
            note: String::new(),
        }
    }

    #[must_use]
    pub fn mode(&self) -> PaletteMode {
        self.mode
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Résultat sélectionné.
    #[must_use]
    pub fn selected(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    /// Comme [`set_items`](Self::set_items), mais la mise en évidence des caractères trouvés n'est
    /// pas calculée : [`fill_visible_highlights`](Self::fill_visible_highlights) le fait pour les
    /// seules lignes affichées (10 au lieu de plusieurs centaines à chaque frappe).
    pub fn set_items_lazy(&mut self, items: Vec<Item>, reset: bool) {
        self.set_items(items, reset);
        self.filled.iter_mut().for_each(|f| *f = false);
    }

    /// Calcule, avec `f`, la mise en évidence des résultats visibles qui n'en ont pas encore.
    /// Renvoie `true` si quelque chose a changé.
    pub fn fill_visible_highlights(&mut self, mut f: impl FnMut(&Item) -> Vec<u32>) -> bool {
        let end = (self.offset + MAX_VISIBLE).min(self.items.len());
        let mut changed = false;
        for i in self.offset..end {
            if !self.filled[i] {
                self.items[i].highlights = f(&self.items[i]);
                self.filled[i] = true;
                changed = true;
            }
        }
        changed
    }

    /// Ajoute des résultats à la suite (recherche dans le projet) sans toucher à la sélection.
    pub fn append_items(&mut self, more: Vec<Item>) {
        self.filled.resize(self.filled.len() + more.len(), true);
        self.items.extend(more);
        self.clamp();
    }

    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = note.into();
    }

    /// Remplace les résultats. `reset` : la saisie vient de changer, la sélection repart du
    /// premier résultat ; sinon (résultats qui arrivent au fil de l'eau) elle reste où elle est.
    pub fn set_items(&mut self, items: Vec<Item>, reset: bool) {
        self.filled = vec![true; items.len()];
        self.items = items;
        if reset {
            self.selected = 0;
            self.offset = 0;
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + MAX_VISIBLE {
            self.offset = self.selected + 1 - MAX_VISIBLE;
        }
        self.offset = self
            .offset
            .min(self.items.len().saturating_sub(MAX_VISIBLE));
    }

    fn byte_of_char(&self, chars: usize) -> usize {
        self.query
            .char_indices()
            .nth(chars)
            .map_or(self.query.len(), |(b, _)| b)
    }

    /// Efface toute la saisie. Renvoie [`Outcome::QueryChanged`] si elle n'était pas vide.
    pub fn clear(&mut self) -> Outcome {
        if self.query.is_empty() {
            return Outcome::None;
        }
        self.query.clear();
        self.caret = 0;
        Outcome::QueryChanged
    }

    /// Traite une touche.
    pub fn handle(&mut self, key: &PaletteKey) -> Outcome {
        let len = self.query.chars().count();
        match key {
            PaletteKey::Text(t) => {
                // Pas de saut de ligne ni de caractère de contrôle dans une ligne de saisie.
                let clean: String = t.chars().filter(|c| !c.is_control()).collect();
                if clean.is_empty() {
                    return Outcome::None;
                }
                let at = self.byte_of_char(self.caret);
                self.query.insert_str(at, &clean);
                self.caret += clean.chars().count();
                Outcome::QueryChanged
            }
            PaletteKey::Backspace => {
                if self.caret == 0 {
                    return Outcome::None;
                }
                let (a, b) = (
                    self.byte_of_char(self.caret - 1),
                    self.byte_of_char(self.caret),
                );
                self.query.replace_range(a..b, "");
                self.caret -= 1;
                Outcome::QueryChanged
            }
            PaletteKey::Delete => {
                if self.caret >= len {
                    return Outcome::None;
                }
                let (a, b) = (
                    self.byte_of_char(self.caret),
                    self.byte_of_char(self.caret + 1),
                );
                self.query.replace_range(a..b, "");
                Outcome::QueryChanged
            }
            PaletteKey::Left => {
                self.caret = self.caret.saturating_sub(1);
                Outcome::Redraw
            }
            PaletteKey::Right => {
                self.caret = (self.caret + 1).min(len);
                Outcome::Redraw
            }
            PaletteKey::Home => {
                self.caret = 0;
                Outcome::Redraw
            }
            PaletteKey::End => {
                self.caret = len;
                Outcome::Redraw
            }
            PaletteKey::Up => self.move_selection(-1),
            PaletteKey::Down => self.move_selection(1),
            PaletteKey::PageUp => self.move_selection(-(MAX_VISIBLE as isize)),
            PaletteKey::PageDown => self.move_selection(MAX_VISIBLE as isize),
            PaletteKey::Enter => {
                if self.items.is_empty() {
                    Outcome::None
                } else {
                    Outcome::Accept
                }
            }
            PaletteKey::Escape => Outcome::Close,
        }
    }

    fn move_selection(&mut self, delta: isize) -> Outcome {
        if self.items.is_empty() {
            return Outcome::None;
        }
        let last = self.items.len() - 1;
        let target = self.selected as isize + delta;
        // Haut / bas bouclent d'un pas ; les sauts de page s'arrêtent aux extrémités.
        self.selected = if delta.abs() == 1 {
            if target < 0 {
                last
            } else if target as usize > last {
                0
            } else {
                target as usize
            }
        } else {
            target.clamp(0, last as isize) as usize
        };
        self.clamp();
        Outcome::Redraw
    }

    /// Sélectionne le résultat affiché à la rangée `visible` (clic). `false` s'il n'existe pas.
    pub fn select_visible(&mut self, visible: usize) -> bool {
        let index = self.offset + visible;
        if index < self.items.len() {
            self.selected = index;
            true
        } else {
            false
        }
    }

    /// Fait défiler la liste (molette) sans changer la sélection.
    pub fn scroll(&mut self, rows: isize) {
        let max = self.items.len().saturating_sub(MAX_VISIBLE);
        self.offset = (self.offset as isize + rows).clamp(0, max as isize) as usize;
    }

    /// Contenu du panneau pour le moteur de rendu. `columns` : nombre de caractères par rangée.
    #[must_use]
    pub fn to_overlay(&self, columns: usize) -> Overlay {
        let columns = columns.max(8);
        let mut rows = Vec::with_capacity(MAX_VISIBLE + 2);

        // Saisie : si elle dépasse la largeur, on garde la fin (là où l'on tape).
        let prompt = self.mode.prompt();
        let full: Vec<char> = prompt.chars().chain(self.query.chars()).collect();
        let caret = prompt.chars().count() + self.caret;
        let room = columns.saturating_sub(1);
        let start = caret.saturating_sub(room);
        let shown: String = full[start..].iter().take(room).collect();
        rows.push(OverlayRow {
            kind: OverlayRowKind::Input,
            text: shown.clone(),
            caret: Some(utf16_len_of_prefix(&shown, caret - start)),
            ..OverlayRow::default()
        });

        let visible = self
            .items
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(MAX_VISIBLE);
        for (i, item) in visible {
            let hint = fit_right(&item.detail, columns / 3);
            let hint_cols = if hint.is_empty() {
                0
            } else {
                hint.chars().count() + 2
            };
            let room = columns.saturating_sub(hint_cols);
            // Les chemins se lisent par la fin (le nom du fichier) ; le reste par le début.
            let from_left = matches!(item.target, Target::File(_) | Target::Match { .. });
            let (text, shift) = fit(&item.label, room, from_left);
            rows.push(OverlayRow {
                kind: if i == self.selected {
                    OverlayRowKind::Selected
                } else {
                    OverlayRowKind::Item
                },
                highlights: highlight_ranges(&text, &item.highlights, shift),
                text,
                hint,
                caret: None,
            });
        }

        let note = if self.items.is_empty() {
            if self.note.is_empty() && !self.query.is_empty() {
                "Aucun résultat".to_owned()
            } else {
                self.note.clone()
            }
        } else if self.items.len() > MAX_VISIBLE {
            let position = format!("{} / {}", self.selected + 1, self.items.len());
            if self.note.is_empty() {
                position
            } else {
                format!("{position} — {}", self.note)
            }
        } else {
            self.note.clone()
        };
        if !note.is_empty() {
            rows.push(OverlayRow {
                kind: OverlayRowKind::Note,
                text: fit(&note, columns, false).0,
                ..OverlayRow::default()
            });
        }
        Overlay { rows }
    }
}

/// Nombre d'unités UTF-16 des `chars` premiers caractères de `s`.
fn utf16_len_of_prefix(s: &str, chars: usize) -> usize {
    s.chars().take(chars).map(char::len_utf16).sum()
}

/// Ajuste `label` à `max` caractères avec une ellipse. Renvoie le texte affiché et le décalage
/// à retrancher aux indices d'origine (`from_left` : on garde la fin, l'ellipse est devant).
fn fit(label: &str, max: usize, from_left: bool) -> (String, usize) {
    let chars: Vec<char> = label.chars().collect();
    if chars.len() <= max {
        return (label.to_owned(), 0);
    }
    if max == 0 {
        return (String::new(), chars.len());
    }
    if from_left {
        let keep = max - 1;
        let shift = chars.len() - keep;
        (
            std::iter::once('\u{2026}')
                .chain(chars[shift..].iter().copied())
                .collect(),
            shift.wrapping_sub(1),
        )
    } else {
        let keep = max - 1;
        (
            chars[..keep]
                .iter()
                .copied()
                .chain(std::iter::once('\u{2026}'))
                .collect(),
            0,
        )
    }
}

/// Texte secondaire tronqué par la gauche (on garde la fin).
fn fit_right(s: &str, max: usize) -> String {
    fit(s, max, true).0
}

/// Convertit des indices de caractères (de l'original) en plages UTF-16 du texte affiché.
/// `shift` : décalage appliqué par [`fit`] (un `wrapping_sub` de 1 signale l'ellipse en tête).
fn highlight_ranges(shown: &str, indices: &[u32], shift: usize) -> Vec<(usize, usize)> {
    let units: Vec<usize> = shown
        .chars()
        .scan(0usize, |acc, c| {
            let start = *acc;
            *acc += c.len_utf16();
            Some(start)
        })
        .collect();
    let total = shown.chars().map(char::len_utf16).sum::<usize>();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in indices {
        // position dans le texte affiché : i - shift (avec l'ellipse en tête, shift = décalage - 1)
        let pos = (i as usize).wrapping_sub(shift);
        let Some(&start) = units.get(pos) else {
            continue;
        };
        if shown.chars().nth(pos) == Some('\u{2026}') {
            continue;
        }
        let end = units.get(pos + 1).copied().unwrap_or(total);
        match out.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    out
}

/// Numéro de ligne demandé par une saisie « 120 » ou « 120:5 » (la colonne est ignorée).
#[must_use]
pub fn parse_goto(query: &str) -> Option<usize> {
    let line = query.trim().split(':').next()?.trim();
    line.parse::<usize>().ok().filter(|n| *n >= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, target: Target) -> Item {
        Item {
            label: label.to_owned(),
            detail: String::new(),
            highlights: Vec::new(),
            target,
        }
    }

    fn files(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| {
                item(
                    &format!("src/file_{i}.rs"),
                    Target::File(format!("src/file_{i}.rs")),
                )
            })
            .collect()
    }

    fn type_text(p: &mut Palette, t: &str) -> Outcome {
        p.handle(&PaletteKey::Text(t.to_owned()))
    }

    #[test]
    fn saisie_et_edition_avec_accents() {
        let mut p = Palette::new(PaletteMode::Files);
        assert_eq!(type_text(&mut p, "é😀a"), Outcome::QueryChanged);
        assert_eq!(p.query(), "é😀a");
        p.handle(&PaletteKey::Left);
        assert_eq!(p.handle(&PaletteKey::Backspace), Outcome::QueryChanged);
        assert_eq!(p.query(), "éa");
        p.handle(&PaletteKey::Home);
        assert_eq!(p.handle(&PaletteKey::Backspace), Outcome::None);
        assert_eq!(p.handle(&PaletteKey::Delete), Outcome::QueryChanged);
        assert_eq!(p.query(), "a");
        p.handle(&PaletteKey::End);
        assert_eq!(p.handle(&PaletteKey::Delete), Outcome::None);
        // les sauts de ligne et caractères de contrôle sont ignorés
        assert_eq!(type_text(&mut p, "\r\n\t"), Outcome::None);
        type_text(&mut p, "x\ny");
        assert_eq!(p.query(), "axy");
    }

    #[test]
    fn effacer_toute_la_saisie() {
        let mut p = Palette::new(PaletteMode::Files);
        assert_eq!(p.clear(), Outcome::None);
        type_text(&mut p, "abc");
        assert_eq!(p.clear(), Outcome::QueryChanged);
        assert_eq!(p.query(), "");
        // le curseur est revenu au début : une nouvelle saisie ne plante pas
        assert_eq!(type_text(&mut p, "x"), Outcome::QueryChanged);
        assert_eq!(p.query(), "x");
    }

    #[test]
    fn selection_boucle_et_la_fenetre_suit() {
        let mut p = Palette::new(PaletteMode::Files);
        p.set_items(files(25), true);
        assert_eq!(p.handle(&PaletteKey::Up), Outcome::Redraw);
        assert_eq!(
            p.selected().unwrap().label,
            "src/file_24.rs",
            "bouclage vers le bas"
        );
        assert_eq!(p.offset, 25 - MAX_VISIBLE);
        p.handle(&PaletteKey::Down);
        assert_eq!(p.selected().unwrap().label, "src/file_0.rs");
        assert_eq!(p.offset, 0);
        for _ in 0..12 {
            p.handle(&PaletteKey::Down);
        }
        assert_eq!(p.selected().unwrap().label, "src/file_12.rs");
        assert!(p.offset > 0 && p.selected >= p.offset && p.selected < p.offset + MAX_VISIBLE);
        // pages : s'arrêtent aux extrémités
        p.handle(&PaletteKey::PageDown);
        p.handle(&PaletteKey::PageDown);
        p.handle(&PaletteKey::PageDown);
        assert_eq!(p.selected, 24);
        p.handle(&PaletteKey::PageUp);
        assert_eq!(p.selected, 14);
    }

    #[test]
    fn nouveaux_resultats_gardent_ou_reinitialisent_la_selection() {
        let mut p = Palette::new(PaletteMode::Files);
        p.set_items(files(20), true);
        for _ in 0..5 {
            p.handle(&PaletteKey::Down);
        }
        // des résultats arrivent au fil de l'eau : la sélection reste
        p.set_items(files(30), false);
        assert_eq!(p.selected, 5);
        // moins de résultats qu'avant : la sélection est ramenée dans la liste
        p.set_items(files(3), false);
        assert_eq!(p.selected, 2);
        // la saisie change : retour au premier
        p.set_items(files(8), true);
        assert_eq!(p.selected, 0);
        p.set_items(Vec::new(), true);
        assert!(p.selected().is_none());
        assert_eq!(p.handle(&PaletteKey::Enter), Outcome::None);
        assert_eq!(p.handle(&PaletteKey::Down), Outcome::None);
    }

    #[test]
    fn validation_fermeture_et_clic() {
        let mut p = Palette::new(PaletteMode::Commands);
        p.set_items(files(15), true);
        assert_eq!(p.handle(&PaletteKey::Enter), Outcome::Accept);
        assert_eq!(p.handle(&PaletteKey::Escape), Outcome::Close);
        p.scroll(3);
        assert_eq!(p.offset, 3);
        assert!(p.select_visible(2));
        assert_eq!(p.selected, 5);
        assert!(!p.select_visible(MAX_VISIBLE + 10));
        p.scroll(-100);
        assert_eq!(p.offset, 0);
        p.scroll(100);
        assert_eq!(p.offset, 5);
    }

    #[test]
    fn rendu_saisie_resultats_et_notes() {
        let mut p = Palette::new(PaletteMode::Commands);
        type_text(&mut p, "undo");
        let o = p.to_overlay(40);
        assert_eq!(o.rows[0].kind, OverlayRowKind::Input);
        assert_eq!(o.rows[0].text, "> undo");
        assert_eq!(o.rows[0].caret, Some(6));
        // sans résultat : message
        assert_eq!(o.rows.last().unwrap().text, "Aucun résultat");

        let mut items = files(12);
        items[0].detail = "Ctrl+Z".into();
        items[0].highlights = vec![0, 1];
        p.set_items(items, true);
        p.set_note("12 résultats");
        let o = p.to_overlay(40);
        // saisie + 10 résultats + note
        assert_eq!(o.rows.len(), 1 + MAX_VISIBLE + 1);
        assert_eq!(o.rows[1].kind, OverlayRowKind::Selected);
        assert_eq!(o.rows[1].hint, "Ctrl+Z");
        assert_eq!(o.rows[1].highlights, vec![(0, 2)]);
        assert_eq!(o.rows[2].kind, OverlayRowKind::Item);
        assert_eq!(o.rows.last().unwrap().text, "1 / 12 — 12 résultats");
    }

    #[test]
    fn les_chemins_longs_gardent_la_fin_et_les_indices_suivent() {
        let mut p = Palette::new(PaletteMode::Files);
        let label = "dossier/sous_dossier/encore_un_dossier/fichier_important.rs";
        // « fichier » commence à l'indice 40 dans le chemin complet
        let start = label.find("fichier").unwrap() as u32;
        p.set_items(
            vec![Item {
                label: label.to_owned(),
                detail: String::new(),
                highlights: (start..start + 7).collect(),
                target: Target::File(label.to_owned()),
            }],
            true,
        );
        let o = p.to_overlay(30);
        let row = &o.rows[1];
        assert_eq!(row.text.chars().count(), 30);
        assert!(row.text.starts_with('\u{2026}') && row.text.ends_with("fichier_important.rs"));
        assert_eq!(row.highlights.len(), 1);
        let (a, b) = row.highlights[0];
        assert_eq!(
            &row.text.chars().skip(a).take(b - a).collect::<String>(),
            "fichier"
        );
    }

    #[test]
    fn la_saisie_longue_garde_la_fin_visible() {
        let mut p = Palette::new(PaletteMode::Search);
        type_text(&mut p, &"a".repeat(60));
        type_text(&mut p, "FIN");
        let o = p.to_overlay(20);
        let row = &o.rows[0];
        assert!(row.text.chars().count() <= 20);
        assert!(row.text.ends_with("FIN"));
        // le curseur reste dans la zone affichée, juste après « FIN »
        assert_eq!(row.caret, Some(row.text.chars().count()));
    }

    #[test]
    fn la_mise_en_evidence_est_calculee_seulement_pour_les_lignes_visibles() {
        let mut p = Palette::new(PaletteMode::Files);
        p.set_items_lazy(files(30), true);
        let mut calls = 0;
        assert!(p.fill_visible_highlights(|_| {
            calls += 1;
            vec![1]
        }));
        assert_eq!(calls, MAX_VISIBLE);
        assert_eq!(p.items()[0].highlights, vec![1]);
        assert!(p.items()[MAX_VISIBLE].highlights.is_empty());
        // rien de nouveau à calculer tant que la fenêtre ne bouge pas
        assert!(!p.fill_visible_highlights(|_| vec![9]));
        // en défilant, les nouvelles lignes visibles sont calculées, pas les anciennes
        p.scroll(5);
        let mut seen = Vec::new();
        p.fill_visible_highlights(|i| {
            seen.push(i.label.clone());
            vec![2]
        });
        assert_eq!(seen.len(), 5);
        assert_eq!(p.items()[0].highlights, vec![1]);
        assert_eq!(p.items()[14].highlights, vec![2]);
        // des résultats ajoutés (recherche) sont considérés comme déjà prêts
        p.append_items(files(3));
        assert!(!p.fill_visible_highlights(|_| vec![7]));
    }

    #[test]
    fn numero_de_ligne() {
        assert_eq!(parse_goto("120"), Some(120));
        assert_eq!(parse_goto(" 7 "), Some(7));
        assert_eq!(parse_goto("42:8"), Some(42));
        assert_eq!(parse_goto("0"), None);
        assert_eq!(parse_goto(""), None);
        assert_eq!(parse_goto("abc"), None);
        assert_eq!(parse_goto("-3"), None);
    }
}
