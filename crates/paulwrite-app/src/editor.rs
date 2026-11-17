//! État d'édition d'une fenêtre : document, vue (première ligne visible), exécution des
//! commandes et construction des rangées à dessiner. Aucune dépendance à winit ni à Win32 :
//! entièrement testable.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use paulwrite_core::motion;
use paulwrite_core::{Document, Motion, Selection, SelectionSet, TextChange, TextEdit};
use paulwrite_render::{FoldMark, MinimapSource, Row, RowSource, Scroll};
use paulwrite_syntax::{kind, Span};

use crate::command::{Command, Effect, LspAction};
use crate::editing::{self, CommentStyle};
use crate::fold::{self, Folds};
use crate::i18n::Lang;
use crate::palette::PaletteMode;
use crate::spans::SpanCache;
use crate::view::{Pos, RowInfo, Walker};

/// Valeur de défilement « trop grand pour décaler les pixels » : l'image est redessinée.
const JUMP: isize = 1 << 20;
/// Repère affiché à la fin d'une ligne repliée.
const FOLD_MARKER: char = '\u{2026}';

/// Parcours de la vue sur un éditeur : n'emprunte que les champs nécessaires (le document en
/// lecture, deux tampons en écriture), le reste de l'éditeur reste utilisable.
macro_rules! walker {
    ($s:expr) => {
        Walker {
            buf: $s.doc.buffer(),
            cols: if $s.wrap && $s.cols > 0 {
                Some($s.cols)
            } else {
                None
            },
            folds: &$s.fold_lines,
            scratch: &mut $s.scratch,
            breaks: &mut $s.breaks,
            fold_marks: $s.fold_marks,
        }
    };
}

/// Texte de composition IME en cours (non encore validé).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// Position du curseur dans `text`, en octets.
    pub cursor: Option<usize>,
}

/// Un soulignement de diagnostic : plage `(ligne, colonne UTF-16)` et gravité (0 erreur,
/// 1 avertissement, 2 information).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Squiggle {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: u8,
}

/// Remplace les sauts de ligne de `text` par ceux du document.
fn normalize_newlines<'a>(text: &'a str, eol: &str) -> Cow<'a, str> {
    if !text.contains(['\r', '\n']) {
        return Cow::Borrowed(text);
    }
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    if eol == "\n" {
        Cow::Owned(unified)
    } else {
        Cow::Owned(unified.replace('\n', eol))
    }
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Éditeur : un document et sa vue.
#[derive(Debug)]
pub struct Editor {
    doc: Document,
    path: Option<PathBuf>,
    saved_revision: u64,
    /// Première ligne affichée et, avec le retour à la ligne, indice de son premier segment.
    first_line: usize,
    first_sub: usize,
    /// Nombre de rangées entières visibles.
    rows: usize,
    /// Largeur de la zone de texte en colonnes (retour à la ligne, défilement horizontal).
    cols: usize,
    /// Retour à la ligne automatique.
    wrap: bool,
    /// Langue des textes de l'éditeur (barre d'état, nom d'un document sans fichier).
    lang: Lang,
    /// Façon de commenter le langage du document (Ctrl+/).
    comment: CommentStyle,
    /// Paires automatiques de parenthèses et de guillemets.
    autopair: bool,
    /// Les flèches ▼ des lignes repliables sont calculées (la souris survole la gouttière).
    fold_marks: bool,
    /// Premier caractère affiché de chaque ligne (sans retour à la ligne).
    hscroll: usize,
    /// Rangées dont la vue a défilé depuis la dernière image dessinée.
    pending_scroll: isize,
    caret_on: bool,
    /// Régions repliées et leur forme en lignes (recalculée quand le texte change).
    folds: Folds,
    fold_lines: Vec<(usize, usize)>,
    fold_dirty: bool,
    /// Modifications en attente pour le thread de coloration.
    syntax_edits: bool,
    pending_edits: Vec<TextEdit>,
    /// Ce que montre chaque rangée (construit par `build_view`).
    layout: Vec<RowInfo>,
    scratch: String,
    breaks: Vec<usize>,
    preedit: Option<Preedit>,
    // Rangées construites par `build_view` (tampons réutilisés d'une image à l'autre).
    row_text: Vec<String>,
    row_sels: Vec<Vec<(usize, usize)>>,
    row_carets: Vec<Vec<usize>>,
    row_underline: Vec<Option<(usize, usize)>>,
    row_spans: Vec<Vec<(usize, usize, u8)>>,
    row_squiggles: Vec<Vec<(usize, usize, u8)>>,
    /// Diagnostics du serveur de langage (positions telles que le serveur les a envoyées).
    squiggles: Vec<Squiggle>,
    ime_anchor: Option<(usize, usize)>,
    /// Ligne du curseur principal (construite par `build_view`).
    cursor_line: usize,
    /// Plages colorées (coloration syntaxique) ; vide si la coloration est inactive.
    spans: SpanCache,
    /// Coloration du document entier, pour la minimap (calculée par le worker au repos).
    overview: SpanCache,
    overview_version: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// Éditeur sur un document vide sans nom.
    #[must_use]
    pub fn new() -> Self {
        Self::from_document(Document::new(), None)
    }

    /// Éditeur sur un document existant.
    #[must_use]
    pub fn from_document(doc: Document, path: Option<PathBuf>) -> Self {
        let saved_revision = doc.buffer().revision();
        let mut doc = doc;
        // Les modifications alimentent le suivi des régions repliées, coloration ou non.
        doc.track_edits(true);
        let _ = doc.take_edits();
        Self {
            doc,
            path,
            saved_revision,
            first_line: 0,
            first_sub: 0,
            rows: 40,
            cols: 0,
            wrap: false,
            lang: Lang::En,
            comment: CommentStyle::default(),
            autopair: true,
            fold_marks: false,
            hscroll: 0,
            pending_scroll: 0,
            folds: Folds::default(),
            fold_lines: Vec::new(),
            fold_dirty: false,
            syntax_edits: false,
            pending_edits: Vec::new(),
            layout: Vec::new(),
            scratch: String::new(),
            breaks: Vec::new(),
            caret_on: true,
            preedit: None,
            row_text: Vec::new(),
            row_sels: Vec::new(),
            row_carets: Vec::new(),
            row_underline: Vec::new(),
            row_spans: Vec::new(),
            row_squiggles: Vec::new(),
            squiggles: Vec::new(),
            ime_anchor: None,
            cursor_line: 0,
            spans: SpanCache::default(),
            overview: SpanCache::default(),
            overview_version: 0,
        }
    }

    /// Remplace le document (fichier chargé). La vue repart du haut.
    pub fn load(&mut self, doc: Document, path: Option<PathBuf>) {
        let (rows, cols, wrap, marks) = (self.rows, self.cols, self.wrap, self.fold_marks);
        let (lang, comment, autopair) = (self.lang, self.comment, self.autopair);
        *self = Self::from_document(doc, path);
        self.lang = lang;
        self.comment = comment;
        self.autopair = autopair;
        self.rows = rows;
        self.cols = cols;
        self.wrap = wrap;
        self.fold_marks = marks;
    }

    // --- Accès -------------------------------------------------------------------------------

    #[must_use]
    pub fn document(&self) -> &Document {
        &self.doc
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.doc.buffer().revision() != self.saved_revision
    }

    /// Mémorise l'état actuel comme « enregistré ».
    pub fn mark_saved(&mut self, revision: u64) {
        self.saved_revision = revision;
    }

    #[must_use]
    pub fn first_line(&self) -> usize {
        self.first_line
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Nom affiché du document.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || self.lang.pick("untitled", "sans titre").to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Titre de la fenêtre.
    #[must_use]
    pub fn title(&self) -> String {
        let star = if self.is_modified() { "*" } else { "" };
        let name = self
            .path
            .as_ref()
            .map_or_else(|| self.display_name(), |p| p.to_string_lossy().into_owned());
        format!("{name}{star} - PaulWrite")
    }

    // --- Coloration syntaxique ---------------------------------------------------------------

    /// Active ou désactive le suivi des modifications pour la coloration syntaxique.
    pub fn enable_syntax(&mut self, on: bool) {
        self.syntax_edits = on;
        if !on {
            self.spans.clear();
            self.overview.clear();
            self.overview_version += 1;
            self.pending_edits.clear();
        }
    }

    /// Active ou désactive le journal des modifications au format LSP (serveur de langage).
    pub fn enable_lsp(&mut self, on: bool) {
        self.doc.track_changes(on);
        if !on {
            self.squiggles.clear();
        }
    }

    /// Modifications au format LSP faites depuis le dernier appel, pour le serveur de langage.
    pub fn take_lsp_changes(&mut self) -> Vec<TextChange> {
        self.doc.take_changes()
    }

    /// Remplace les diagnostics affichés.
    pub fn set_squiggles(&mut self, squiggles: Vec<Squiggle>) {
        self.squiggles = squiggles;
    }

    #[must_use]
    pub fn squiggles(&self) -> &[Squiggle] {
        &self.squiggles
    }

    /// Position du curseur principal au format LSP : (ligne, colonne UTF-16).
    #[must_use]
    pub fn caret_lsp_position(&self) -> (usize, usize) {
        self.doc
            .buffer()
            .lsp_position(self.doc.selections().primary().head)
    }

    /// Nombre de caractères d'identifiant (lettres, chiffres, `_`) juste avant le curseur
    /// principal, sur sa ligne : ce que la complétion remplace.
    #[must_use]
    pub fn word_prefix_chars(&self) -> usize {
        let buf = self.doc.buffer();
        let head = self.doc.selections().primary().head;
        let line_start = buf.line_start(buf.char_to_line(head));
        (line_start..head)
            .rev()
            .take_while(|&i| {
                let c = buf.char(i);
                c.is_alphanumeric() || c == '_'
            })
            .count()
    }

    /// Insère `text` à la place des `prefix` caractères qui précèdent le curseur principal
    /// (validation d'un élément de complétion).
    pub fn complete(&mut self, prefix: usize, text: &str, now_ms: u64) {
        let head = self.doc.selections().primary().head;
        let from = head.saturating_sub(prefix);
        self.doc
            .set_selections(SelectionSet::single(Selection::new(from, head)));
        self.insert_text(text, now_ms);
    }

    /// Révision du texte (voir `Buffer::revision`).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.doc.buffer().revision()
    }

    /// Modifications depuis le dernier appel, pour le thread de coloration. Les plages colorées
    /// actuelles sont décalées en conséquence, pour que les couleurs ne sautent pas en attendant
    /// la réponse du worker.
    pub fn take_edits(&mut self) -> Vec<TextEdit> {
        self.sync();
        let edits = std::mem::take(&mut self.pending_edits);
        for e in &edits {
            self.spans.apply_edit(e);
            // la minimap garde ses couleurs pendant la frappe, décalées comme le texte
            self.overview.apply_edit(e);
        }
        edits
    }

    /// Prend en compte les modifications du texte : les régions repliées suivent, et leur forme
    /// en lignes est recalculée si le texte a changé.
    fn sync(&mut self) {
        let regions = self.folds.len();
        let mut lines_moved = false;
        for e in self.doc.take_edits() {
            // une modification qui garde le même nombre de lignes ne déplace aucune région en
            // lignes (seulement en octets) : inutile de tout recalculer à chaque frappe
            lines_moved |= e.new_end_point.0.saturating_sub(e.start_point.0)
                != e.old_end_point.0.saturating_sub(e.start_point.0);
            self.folds.apply_edit(&e);
            if self.syntax_edits {
                self.pending_edits.push(e);
            }
        }
        if self.fold_dirty || lines_moved || self.folds.len() != regions {
            self.fold_dirty = false;
            if self.folds.is_empty() {
                self.fold_lines.clear();
            } else {
                self.folds.lines(self.doc.buffer(), &mut self.fold_lines);
            }
        }
    }

    /// Dernière ligne affichée dans la vue.
    fn last_visible_line(&mut self) -> usize {
        let from = self.first_pos();
        let rows = self.rows;
        let mut w = walker!(self);
        w.advance(from, rows.saturating_sub(1)).0.line
    }

    /// Plage d'octets des lignes visibles, élargie de `margin` lignes de chaque côté.
    #[must_use]
    pub fn view_bytes(&mut self, margin: usize) -> std::ops::Range<usize> {
        self.sync();
        let last = if self.wrap_cols().is_none() && self.fold_lines.is_empty() {
            self.first_line + self.rows
        } else {
            self.last_visible_line() + 1
        };
        let rope = self.doc.buffer().rope();
        let total = rope.len_lines();
        let from = self.first_line.saturating_sub(margin).min(total);
        let to = (last + margin).min(total);
        let byte = |line: usize| {
            if line >= total {
                rope.len_bytes()
            } else {
                rope.line_to_byte(line)
            }
        };
        byte(from)..byte(to)
    }

    /// Les plages colorées couvrent les lignes visibles (avec une marge de 30 lignes)
    /// : sinon il faut demander une nouvelle fenêtre au worker.
    #[must_use]
    pub fn spans_cover_view(&mut self) -> bool {
        let range = self.view_bytes(30);
        self.spans.covers(&range)
    }

    /// Reçoit la coloration du document entier (minimap).
    pub fn set_overview(&mut self, range: std::ops::Range<usize>, spans: Vec<Span>) {
        self.overview.set(range, spans);
        self.overview_version += 1;
    }

    /// Change à chaque nouvelle coloration de la minimap (clé du cache de son image).
    #[must_use]
    pub fn overview_version(&self) -> u64 {
        self.overview_version
    }

    /// Reçoit les plages colorées calculées par le worker pour `range`.
    pub fn set_spans(&mut self, range: std::ops::Range<usize>, spans: Vec<Span>) {
        self.spans.set(range, spans);
    }

    // --- Vue ---------------------------------------------------------------------------------

    /// Fixe le nombre de rangées entières visibles.
    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
        self.clamp_first_line();
        self.ensure_caret_visible();
    }

    /// Fixe la largeur de la zone de texte en colonnes. Elle décide des retours à la ligne.
    pub fn set_viewport_cols(&mut self, cols: usize) {
        if cols == self.cols {
            return;
        }
        self.cols = cols;
        self.relayout();
    }

    /// Largeur de la zone de texte en colonnes.
    #[must_use]
    pub fn viewport_cols(&self) -> usize {
        self.cols
    }

    /// Active ou désactive le retour à la ligne automatique.
    pub fn set_wrap(&mut self, on: bool) {
        if self.wrap == on {
            return;
        }
        self.wrap = on;
        self.relayout();
    }

    /// Le retour à la ligne automatique est actif.
    #[must_use]
    pub fn wrap(&self) -> bool {
        self.wrap
    }

    /// Façon de commenter le langage du document.
    pub fn set_comment_style(&mut self, style: CommentStyle) {
        self.comment = style;
    }

    /// Active ou coupe les paires automatiques de signes.
    pub fn set_autopair(&mut self, on: bool) {
        self.autopair = on;
    }

    /// Langue des textes de l'éditeur.
    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
    }

    /// Calcule (ou non) les flèches ▼ des lignes repliables : à activer pendant que la souris
    /// survole la gouttière, car les trouver coûte une lecture de quelques lignes par rangée.
    pub fn set_fold_marks(&mut self, on: bool) {
        self.fold_marks = on;
    }

    /// Largeur de coupure (colonnes), `None` sans retour à la ligne.
    fn wrap_cols(&self) -> Option<usize> {
        (self.wrap && self.cols > 0).then_some(self.cols)
    }

    /// La disposition des rangées a changé (largeur, retour à la ligne) : tout est redessiné.
    fn relayout(&mut self) {
        self.sync();
        let first = self.first_pos();
        let first = walker!(self).normalize(first);
        self.first_line = first.line;
        self.first_sub = first.sub;
        if self.wrap_cols().is_some() {
            self.hscroll = 0;
        }
        self.pending_scroll = JUMP;
        self.ensure_caret_visible();
    }

    fn first_pos(&self) -> Pos {
        Pos {
            line: self.first_line,
            sub: self.first_sub,
        }
    }

    fn set_first_pos(&mut self, pos: Pos) {
        self.first_line = pos.line;
        self.first_sub = pos.sub;
    }

    fn max_first_line(&self) -> usize {
        self.doc.buffer().len_lines().saturating_sub(1)
    }

    fn clamp_first_line(&mut self) {
        self.first_line = self.first_line.min(self.max_first_line());
        if self.wrap_cols().is_none() {
            self.first_sub = 0;
        }
    }

    /// Place le curseur à la ligne `line` (à partir de 1) et à la colonne `col` (en caractères,
    /// à partir de 0) et centre la vue dessus. Les valeurs hors du document sont ramenées dedans.
    pub fn goto(&mut self, line: usize, col: usize) {
        let buf = self.doc.buffer();
        let line0 = line
            .saturating_sub(1)
            .min(buf.len_lines().saturating_sub(1));
        let pos = buf.line_start(line0) + col.min(buf.line_len(line0));
        self.doc
            .set_selections(SelectionSet::single(Selection::caret(pos)));
        self.caret_on = true;
        self.scroll_to_center(line0);
        self.ensure_caret_visible();
    }

    /// Texte sélectionné (sélections jointes par des sauts de ligne), tronqué à `max_bytes`.
    #[must_use]
    pub fn selection_string(&self, max_bytes: usize) -> String {
        let mut s = self.selected_text().unwrap_or_default();
        if s.len() > max_bytes {
            let mut end = max_bytes;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
        s
    }

    /// Ligne et colonne (à partir de 1, en caractères) du curseur principal.
    #[must_use]
    pub fn caret_line_col(&self) -> (usize, usize) {
        let head = self.doc.selections().primary().head;
        let (line, col) = self.doc.buffer().char_to_line_col(head);
        (line + 1, col + 1)
    }

    /// Nombre de lignes du document.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.doc.buffer().len_lines()
    }

    /// Une ligne = une rangée et aucune ligne masquée : les positions se calculent sans parcours.
    fn plain_rows(&self) -> bool {
        self.wrap_cols().is_none() && self.fold_lines.is_empty()
    }

    /// Fait défiler la vue de `rows` rangées (positif : vers le bas du document).
    pub fn scroll_by(&mut self, rows: isize) {
        self.sync();
        let from = self.first_pos();
        let (to, moved) = if self.plain_rows() {
            let line =
                (from.line as isize + rows).clamp(0, self.max_first_line() as isize) as usize;
            (Pos { line, sub: 0 }, line.abs_diff(from.line))
        } else {
            let mut w = walker!(self);
            if rows >= 0 {
                w.advance(from, rows.unsigned_abs())
            } else {
                w.retreat(from, rows.unsigned_abs())
            }
        };
        self.set_first_pos(to);
        self.pending_scroll += if rows >= 0 {
            moved as isize
        } else {
            -(moved as isize)
        };
    }

    /// Nombre de rangées de `from` à `to` (`to` après `from`), `None` au-delà de `limit`.
    fn rows_between(&mut self, from: Pos, to: Pos, limit: usize) -> Option<usize> {
        let mut w = walker!(self);
        let mut pos = from;
        for n in 0..=limit {
            if pos >= to {
                return Some(n);
            }
            let (next, moved) = w.advance(pos, 1);
            if moved == 0 {
                return None;
            }
            pos = next;
        }
        None
    }

    /// Déplace la vue pour que le curseur principal soit visible (et déplie les régions qui le
    /// masquent).
    pub fn ensure_caret_visible(&mut self) {
        self.sync();
        if !self.folds.is_empty() {
            let buf = self.doc.buffer();
            let mut changed = false;
            for s in self.doc.selections().iter() {
                changed |= self.folds.reveal_char(buf, s.head);
            }
            if changed {
                self.fold_dirty = true;
                self.sync();
            }
        }
        let head = self.doc.selections().primary().head;
        let (line, col) = self.doc.buffer().char_to_line_col(head);
        let rows = self.rows;
        if self.plain_rows() {
            // chemin ordinaire (une ligne par rangée) : de l'arithmétique
            let before = self.first_line;
            if line < self.first_line {
                self.first_line = line;
            } else if line >= self.first_line + rows {
                self.first_line = line + 1 - rows;
            }
            self.pending_scroll += self.first_line as isize - before as isize;
        } else {
            let sub = walker!(self).segment_of(line, col);
            let caret = Pos { line, sub };
            let first = self.first_pos();
            if caret < first {
                let d = self.rows_between(caret, first, rows);
                self.set_first_pos(caret);
                self.pending_scroll -= d.map_or(JUMP, |d| d as isize);
            } else {
                let last = walker!(self).advance(first, rows.saturating_sub(1)).0;
                if caret > last {
                    match self.rows_between(last, caret, rows) {
                        Some(k) => {
                            let to = walker!(self).advance(first, k).0;
                            self.set_first_pos(to);
                            self.pending_scroll += k as isize;
                        }
                        None => {
                            let to = walker!(self).retreat(caret, rows.saturating_sub(1)).0;
                            self.set_first_pos(to);
                            self.pending_scroll = JUMP;
                        }
                    }
                }
            }
        }
        if self.wrap_cols().is_none() {
            self.follow_caret_horizontally(line, col);
        }
    }

    /// Défilement horizontal minimal pour que la colonne `col` de `line` reste affichée.
    fn follow_caret_horizontally(&mut self, line: usize, col: usize) {
        let cols = self.cols;
        if cols == 0 {
            return;
        }
        let before = self.hscroll;
        // on défile par à-coups d'un quart de largeur : taper au bord de la fenêtre ne redessine
        // pas toutes les rangées à chaque frappe
        let jump = (cols / 4).max(1);
        if col < self.hscroll {
            self.hscroll = col.saturating_sub(jump);
        } else {
            // largeur affichée entre le premier caractère visible et le curseur
            let buf = self.doc.buffer();
            let start = buf.line_start(line);
            let width =
                |h: usize| crate::wrap::visual_width(buf.slice(start + h..start + col).chars());
            let mut h = self.hscroll;
            // curseur gardé à 8 colonnes du bord droit
            let mut guard = 0;
            while width(h) + 8 > cols && h < col && guard < 4096 {
                h += (width(h) + 8 - cols).max(1) + jump;
                guard += 1;
            }
            self.hscroll = h.min(col);
        }
        if self.hscroll != before {
            self.pending_scroll = JUMP;
        }
    }

    /// Fait défiler horizontalement de `cols` caractères (sans retour à la ligne).
    pub fn scroll_horizontally(&mut self, cols: isize) {
        if self.wrap_cols().is_some() {
            return;
        }
        let target = (self.hscroll as isize + cols).max(0) as usize;
        if target != self.hscroll {
            self.hscroll = target;
            self.pending_scroll = JUMP;
        }
    }

    /// Premier caractère affiché de chaque ligne (défilement horizontal).
    #[must_use]
    pub fn hscroll(&self) -> usize {
        if self.wrap_cols().is_some() {
            0
        } else {
            self.hscroll
        }
    }

    /// De combien de rangées la vue a défilé depuis la dernière image dessinée.
    #[must_use]
    pub fn scrolled_since_draw(&self) -> isize {
        self.pending_scroll
    }

    /// À appeler après avoir dessiné une image.
    pub fn mark_drawn(&mut self) {
        self.pending_scroll = 0;
    }

    /// Allume/éteint le curseur (clignotement). Renvoie `true` si l'état a changé.
    pub fn set_caret_visible(&mut self, on: bool) -> bool {
        let changed = self.caret_on != on;
        self.caret_on = on;
        changed
    }

    #[must_use]
    pub fn caret_visible(&self) -> bool {
        self.caret_on
    }

    pub fn set_preedit(&mut self, preedit: Option<Preedit>) {
        self.preedit = preedit.filter(|p| !p.text.is_empty());
    }

    // --- Commandes ---------------------------------------------------------------------------

    /// Exécute une commande. Renvoie un effet que l'application doit réaliser, s'il y en a un.
    pub fn execute(&mut self, cmd: Command, now_ms: u64) -> Option<Effect> {
        self.caret_on = true;
        match cmd {
            Command::Move(m) => self.move_by(m, false),
            Command::Extend(m) => self.move_by(m, true),
            Command::Fold => self.fold_at_caret(),
            Command::Unfold => self.unfold_at_caret(),
            Command::FoldAll => self.fold_all(),
            Command::UnfoldAll => self.unfold_all(),
            Command::ToggleWordWrap => return Some(Effect::ToggleWordWrap),
            Command::SetLanguage(l) => return Some(Effect::SetLanguage(l)),
            Command::PageUp(extend) => self.page(false, extend),
            Command::PageDown(extend) => self.page(true, extend),
            Command::ScrollUp => self.scroll_by(-1),
            Command::ScrollDown => self.scroll_by(1),
            Command::InsertNewline => {
                editing::newline_with_indent(&mut self.doc, now_ms);
                self.ensure_caret_visible();
            }
            Command::InsertTab => {
                editing::insert_tab(&mut self.doc, now_ms);
                self.ensure_caret_visible();
            }
            Command::DeleteBackward => {
                if !(self.autopair && editing::backspace_pair(&mut self.doc, now_ms)) {
                    self.doc.delete_backward(now_ms);
                }
                self.ensure_caret_visible();
            }
            Command::DuplicateLine => {
                editing::duplicate_lines(&mut self.doc, now_ms);
                self.ensure_caret_visible();
            }
            Command::DeleteLine => {
                editing::delete_lines(&mut self.doc, now_ms);
                self.ensure_caret_visible();
            }
            Command::MoveLineUp => {
                editing::move_lines(&mut self.doc, true, now_ms);
                self.ensure_caret_visible();
            }
            Command::MoveLineDown => {
                editing::move_lines(&mut self.doc, false, now_ms);
                self.ensure_caret_visible();
            }
            Command::ToggleComment => {
                editing::toggle_comment(&mut self.doc, self.comment, now_ms);
                self.ensure_caret_visible();
            }
            Command::Indent => {
                editing::indent_lines(&mut self.doc, now_ms);
                self.ensure_caret_visible();
            }
            Command::Unindent => {
                editing::unindent_lines(&mut self.doc, now_ms);
                self.ensure_caret_visible();
            }
            Command::SelectLine => {
                editing::select_lines(&mut self.doc);
                self.ensure_caret_visible();
            }
            Command::SelectNextOccurrence => {
                editing::select_next_occurrence(&mut self.doc);
                self.ensure_caret_visible();
            }
            Command::DeleteForward => {
                self.doc.delete_forward(now_ms);
                self.ensure_caret_visible();
            }
            Command::Undo => {
                self.doc.undo();
                self.ensure_caret_visible();
            }
            Command::Redo => {
                self.doc.redo();
                self.ensure_caret_visible();
            }
            Command::SelectAll => self.doc.select_all(),
            Command::CollapseSelections => {
                let head = self.doc.selections().primary().head;
                self.doc
                    .set_selections(SelectionSet::single(Selection::caret(head)));
            }
            Command::AddCursorAbove => self.add_cursor(false),
            Command::AddCursorBelow => self.add_cursor(true),
            Command::Copy => return self.selected_text().map(Effect::SetClipboard),
            Command::Cut => {
                let text = self.selected_text()?;
                self.doc.delete_backward(now_ms);
                self.ensure_caret_visible();
                return Some(Effect::SetClipboard(text));
            }
            Command::Paste => return Some(Effect::RequestPaste),
            Command::Save => return Some(Effect::Save),
            Command::SaveAs => return Some(Effect::SaveAs),
            Command::Open => return Some(Effect::Open),
            Command::NewFile => return Some(Effect::NewTab),
            Command::NewWindow => return Some(Effect::NewWindow),
            Command::CloseTab => return Some(Effect::CloseTab),
            Command::NextTab => return Some(Effect::NextTab),
            Command::PrevTab => return Some(Effect::PrevTab),
            Command::GotoFile => return Some(Effect::OpenPalette(PaletteMode::Files)),
            Command::CommandPalette => return Some(Effect::OpenPalette(PaletteMode::Commands)),
            Command::GotoLine => return Some(Effect::OpenPalette(PaletteMode::GotoLine)),
            Command::SearchProject => return Some(Effect::OpenPalette(PaletteMode::Search)),
            Command::ReloadPlugins => return Some(Effect::ReloadPlugins),
            Command::OpenFolder => return Some(Effect::OpenFolder),
            Command::CloseFolder => return Some(Effect::CloseFolder),
            Command::ToggleSidebar => return Some(Effect::ToggleSidebar),
            Command::OpenSettings => return Some(Effect::OpenSettings),
            Command::About => return Some(Effect::About),
            Command::LspCompletion => return Some(Effect::Lsp(LspAction::Completion)),
            Command::LspDefinition => return Some(Effect::Lsp(LspAction::Definition)),
            Command::LspHover => return Some(Effect::Lsp(LspAction::Hover)),
            Command::LspDiagnostics => return Some(Effect::Lsp(LspAction::Diagnostics)),
            Command::Quit => return Some(Effect::Quit),
        }
        None
    }

    /// Insère du texte à chaque curseur (frappe, validation IME, collage).
    pub fn insert_text(&mut self, text: &str, now_ms: u64) {
        self.caret_on = true;
        // un seul caractère tapé : paires automatiques (parenthèses, guillemets)
        let mut chars = text.chars();
        if let (true, Some(c), None) = (self.autopair, chars.next(), chars.next()) {
            if editing::type_char(&mut self.doc, c, now_ms) {
                self.ensure_caret_visible();
                return;
            }
        }
        let text = normalize_newlines(text, self.doc.line_ending().as_str());
        if self.doc.insert_text(&text, now_ms) {
            self.ensure_caret_visible();
        }
    }

    /// Déplace (ou étend) les curseurs. Haut et bas suivent les rangées affichées quand il y a des
    /// retours à la ligne ou des lignes repliées.
    fn move_by(&mut self, m: Motion, extend: bool) {
        let visual =
            matches!(m, Motion::Up | Motion::Down) && self.move_visual(m == Motion::Down, extend);
        if !visual {
            self.doc.move_selections(m, extend);
        }
        self.ensure_caret_visible();
    }

    /// Déplace le curseur unique d'une rangée affichée vers le haut ou le bas, en gardant sa
    /// colonne dans la rangée. `false` : rien à faire ici (le déplacement par ligne s'en charge).
    fn move_visual(&mut self, down: bool, extend: bool) -> bool {
        if self.plain_rows() || self.doc.selections().len() != 1 {
            return false;
        }
        self.sync();
        let sel = self.doc.selections().primary();
        let buf = self.doc.buffer();
        let (line, col) = buf.char_to_line_col(sel.head);
        let mut w = walker!(self);
        let sub = w.segment_of(line, col);
        let goal = sel.goal_col.unwrap_or(col - w.breaks[sub]);
        let from = Pos { line, sub };
        let (target, moved) = if down {
            w.advance(from, 1)
        } else {
            w.retreat(from, 1)
        };
        if moved == 0 {
            return false;
        }
        w.segments(target.line);
        let a = w.breaks[target.sub];
        let last = target.sub + 1 == w.breaks.len();
        let b = w
            .breaks
            .get(target.sub + 1)
            .copied()
            .unwrap_or(buf.line_len(target.line));
        let room = if last {
            b - a
        } else {
            (b - a).saturating_sub(1)
        };
        let head = buf.line_start(target.line) + a + goal.min(room);
        self.doc.move_head_to(head, extend, goal);
        true
    }

    fn page(&mut self, down: bool, extend: bool) {
        let n = self.rows.saturating_sub(1).max(1);
        let motion = if down { Motion::Down } else { Motion::Up };
        for _ in 0..n {
            if !self.move_visual(down, extend) {
                self.doc.move_selections(motion, extend);
            }
        }
        if down {
            self.scroll_by(n as isize);
        } else {
            self.scroll_by(-(n as isize));
        }
        self.ensure_caret_visible();
    }

    fn add_cursor(&mut self, down: bool) {
        let buf = self.doc.buffer();
        let head = self.doc.selections().primary().head;
        let col = buf.char_to_line_col(head).1;
        let pos = motion::vertical(buf, head, col, down);
        if pos != head {
            self.doc.add_cursor(pos);
            self.ensure_caret_visible();
        }
    }

    /// Texte sélectionné (une sélection par ligne en multi-curseurs), `None` si rien n'est
    /// sélectionné.
    fn selected_text(&self) -> Option<String> {
        let buf = self.doc.buffer();
        let mut out = String::new();
        let mut any = false;
        for sel in self.doc.selections() {
            if sel.is_empty() {
                continue;
            }
            if any {
                out.push('\n');
            }
            any = true;
            for chunk in buf.slice(sel.start()..sel.end()).chunks() {
                out.push_str(chunk);
            }
        }
        any.then_some(out)
    }

    // --- Souris ------------------------------------------------------------------------------

    /// Place le curseur à la rangée `row` de la vue, colonne `char_col` (en caractères). Avec
    /// `extend`, étend la sélection du curseur principal (glisser, Maj+clic).
    pub fn click(&mut self, row: usize, char_col: usize, extend: bool) {
        self.build_layout();
        let info = self.layout[row.min(self.layout.len() - 1)];
        let buf = self.doc.buffer();
        let total = buf.len_lines();
        let line = info.line.min(total - 1);
        // au-delà de la fin du document : dernière ligne entière
        let (start, end, last) = if info.line >= total {
            (0, buf.line_len(line), true)
        } else {
            (info.start, info.end, info.last)
        };
        // la fin d'un segment coupé est le début du suivant : on ne s'y arrête pas
        let room = if last {
            end - start
        } else {
            (end - start).saturating_sub(1)
        };
        let pos = buf.line_start(line) + start + char_col.min(room);
        let anchor = if extend {
            self.doc.selections().primary().anchor
        } else {
            pos
        };
        self.doc
            .set_selections(SelectionSet::single(Selection::new(anchor, pos)));
        self.caret_on = true;
    }

    // --- Interface autour du texte -----------------------------------------------------------

    /// Nombre de chiffres du plus grand numéro de ligne.
    #[must_use]
    pub fn line_digits(&self) -> usize {
        self.line_count().max(1).ilog10() as usize + 1
    }

    /// Position de la vue dans le document.
    #[must_use]
    pub fn scroll(&self) -> Scroll {
        let total = self.line_count();
        // lignes distinctes de la vue (plus de rangées que de lignes avec le retour à la ligne)
        let visible = if self.plain_rows() || self.layout.is_empty() {
            self.rows
        } else {
            self.layout
                .iter()
                .filter(|r| r.first && r.line < total)
                .count()
                .max(1)
        };
        Scroll {
            first: self.first_line,
            visible,
            total,
        }
    }

    /// Texte du document pour la minimap.
    #[must_use]
    pub fn minimap(&self) -> BufferLines<'_> {
        BufferLines(self.doc.buffer(), &self.overview)
    }

    /// Place la première ligne visible (sans bouger le curseur).
    pub fn set_first_line(&mut self, line: usize) {
        self.sync();
        let line = line.min(self.max_first_line());
        if self.plain_rows() {
            self.pending_scroll += line as isize - self.first_line as isize;
            self.first_line = line;
            self.first_sub = 0;
        } else {
            let pos = walker!(self).normalize(Pos { line, sub: 0 });
            self.set_first_pos(pos);
            self.pending_scroll = JUMP;
        }
    }

    /// Fait défiler la vue pour que `line` soit au milieu de la fenêtre.
    pub fn scroll_to_center(&mut self, line: usize) {
        if self.plain_rows() {
            self.set_first_line(line.saturating_sub(self.rows / 2));
        } else {
            self.sync();
            let half = self.rows / 2;
            let line = line.min(self.max_first_line());
            let mut w = walker!(self);
            let target = w.normalize(Pos { line, sub: 0 });
            let first = w.retreat(target, half).0;
            self.set_first_pos(first);
            self.pending_scroll = JUMP;
        }
    }

    /// Fait défiler la vue à une fraction (0..1) du document : barre de défilement.
    pub fn scroll_to_fraction(&mut self, fraction: f32) {
        let last = self.max_first_line() as f32;
        self.set_first_line((fraction.clamp(0.0, 1.0) * last).round() as usize);
    }

    /// Sélectionne la ligne de la rangée `row` en entier, fin de ligne comprise (clic dans la
    /// gouttière).
    pub fn select_row(&mut self, row: usize) {
        self.build_layout();
        let info = self.layout[row.min(self.layout.len() - 1)];
        let buf = self.doc.buffer();
        let line = info.line.min(buf.len_lines().saturating_sub(1));
        let start = buf.line_start(line);
        let end = if line + 1 < buf.len_lines() {
            buf.line_start(line + 1)
        } else {
            buf.len_chars()
        };
        self.doc
            .set_selections(SelectionSet::single(Selection::new(start, end)));
        self.caret_on = true;
    }

    /// Texte de la barre d'état : position du curseur ou taille de la sélection, comme Sublime
    /// Text (« Line 3, Column 7 », « 12 characters selected », « 3 selection regions »).
    pub fn write_status(&self, out: &mut String) {
        use std::fmt::Write as _;
        let buf = self.doc.buffer();
        let sels = self.doc.selections();
        if sels.len() > 1 {
            let n = sels.len();
            let _ = match self.lang {
                Lang::En => write!(out, "{n} selection regions"),
                Lang::Fr => write!(out, "{n} zones de sélection"),
            };
            return;
        }
        let sel = sels.primary();
        if sel.is_empty() {
            let line = buf.char_to_line(sel.head);
            let upto = sel.head - buf.line_start(line);
            let mut col = 0usize;
            for c in buf.line(line).chars().take(upto) {
                col = if c == '\t' {
                    (col / 4 + 1) * 4
                } else {
                    col + 1
                };
            }
            let (line, col) = (line + 1, col + 1);
            let _ = match self.lang {
                Lang::En => write!(out, "Line {line}, Column {col}"),
                Lang::Fr => write!(out, "Ligne {line}, colonne {col}"),
            };
        } else {
            let chars = sel.end() - sel.start();
            let lines = buf.char_to_line(sel.end()) - buf.char_to_line(sel.start()) + 1;
            let (en, fr) = if chars == 1 { ("", "") } else { ("s", "s") };
            let _ = match (self.lang, lines > 1) {
                (Lang::En, true) => write!(out, "{lines} lines, {chars} character{en} selected"),
                (Lang::En, false) => write!(out, "{chars} character{en} selected"),
                (Lang::Fr, true) => {
                    write!(out, "{lines} lignes, {chars} caractère{fr} sélectionné{fr}")
                }
                (Lang::Fr, false) => write!(out, "{chars} caractère{fr} sélectionné{fr}"),
            };
        }
    }

    // --- Construction des rangées ------------------------------------------------------------

    /// Prépare les rangées visibles. À appeler avant de dessiner.
    pub fn build_view(&mut self) {
        self.build_layout();
        let rows = self.rows;
        self.row_text.resize_with(rows, String::new);
        self.row_sels.resize_with(rows, Vec::new);
        self.row_carets.resize_with(rows, Vec::new);
        self.row_underline.resize(rows, None);
        self.row_spans.resize_with(rows, Vec::new);
        self.row_squiggles.resize_with(rows, Vec::new);

        let buf = self.doc.buffer();
        let total = buf.len_lines();
        self.cursor_line = buf.char_to_line(self.doc.selections().primary().head);

        for r in 0..rows {
            let s = &mut self.row_text[r];
            s.clear();
            self.row_sels[r].clear();
            self.row_carets[r].clear();
            self.row_underline[r] = None;
            self.row_spans[r].clear();
            self.row_squiggles[r].clear();
            let info = self.layout[r];
            let line = info.line;
            if line >= total {
                continue;
            }
            // le texte de la rangée : un segment de la ligne, sans fin de ligne
            let ls = info.line_start;
            for chunk in buf.line(line).slice(info.start..info.end).chunks() {
                s.push_str(chunk);
            }
            // Diagnostics : positions LSP (ligne, UTF-16) relatives au début de la rangée.
            if !self.squiggles.is_empty() {
                let units = utf16_len(s);
                let skipped = if info.start > 0 {
                    buf.char_to_utf16(ls + info.start) - buf.char_to_utf16(ls)
                } else {
                    0
                };
                for q in &self.squiggles {
                    if q.start.0 > line || q.end.0 < line {
                        continue;
                    }
                    let a = if q.start.0 == line { q.start.1 } else { 0 };
                    let b = if q.end.0 == line { q.end.1 } else { usize::MAX };
                    if b < skipped || a > skipped + units {
                        continue;
                    }
                    let rel = |v: usize| v.saturating_sub(skipped).min(units);
                    // une plage qui touche seulement le bord de la rangée n'y laisse rien
                    if rel(a) == rel(b) && a != b {
                        continue;
                    }
                    self.row_squiggles[r].push((rel(a), rel(b), q.severity));
                }
            }
            // Coloration : plages d'octets du document -> positions UTF-16 dans la rangée.
            if !self.spans.is_empty() {
                let bs = buf.char_to_byte(ls + info.start);
                let be = bs + s.len();
                for sp in self.spans.overlapping(bs, be) {
                    let a = sp.start.max(bs) - bs;
                    let b = sp.end.min(be) - bs;
                    if a < b {
                        self.row_spans[r].push((utf16_of_byte(s, a), utf16_of_byte(s, b), sp.kind));
                    }
                }
            }
        }

        // Décorations : sélections et curseurs, rangée par rangée.
        self.ime_anchor = None;
        let primary = self.doc.selections().primary_index();
        let sels = self.doc.selections().as_slice();
        for r in 0..rows {
            let info = self.layout[r];
            if info.line >= total {
                continue;
            }
            let ls = info.line_start;
            let (from, to) = (ls + info.start, ls + info.end);
            let text = &self.row_text[r];
            // longueur UTF-16 du texte, calculée seulement si une décoration en a besoin
            let mut text_units: Option<usize> = None;
            // les sélections sont triées : on part de la première qui n'est pas avant la rangée
            let first_sel = sels.partition_point(|s| s.end() < from);
            for (i, sel) in sels.iter().enumerate().skip(first_sel) {
                if sel.start() > to {
                    break;
                }
                if !sel.is_empty() {
                    let a = sel.start().max(from);
                    let b = sel.end().min(to);
                    // le saut de ligne est sélectionné : la surbrillance déborde d'un caractère
                    let includes_eol = info.last && sel.end() > to && info.line + 1 < total;
                    if b > a || includes_eol {
                        let ua = utf16_of_char_col(text, a - from);
                        let ub = if includes_eol {
                            *text_units.get_or_insert_with(|| utf16_len(text)) + 1
                        } else {
                            utf16_of_char_col(text, b - from)
                        };
                        self.row_sels[r].push((ua, ub));
                    }
                }
                // un curseur au bord d'un segment coupé s'affiche au début du suivant
                let head = sel.head;
                if head >= from && (head < to || (head == to && info.last)) {
                    let u = utf16_of_char_col(text, head - from);
                    if self.caret_on {
                        self.row_carets[r].push(u);
                    }
                    if i == primary {
                        self.ime_anchor = Some((r, u));
                    }
                }
            }
            // repère « … » des lignes repliées, en couleur de commentaire
            if info.folded {
                let units = *text_units.get_or_insert_with(|| utf16_len(text));
                self.row_spans[r].push((units, units + 1, kind::COMMENT));
                self.row_text[r].push(FOLD_MARKER);
            }
        }

        // Composition IME : texte inséré visuellement au curseur principal, souligné.
        if let (Some(pre), Some((r, u))) = (&self.preedit, self.ime_anchor) {
            let text = &mut self.row_text[r];
            let byte = byte_of_utf16(text, u);
            text.insert_str(byte, &pre.text);
            let end = u + utf16_len(&pre.text);
            self.row_sels[r].clear();
            self.row_carets[r].clear();
            self.row_spans[r].clear();
            self.row_squiggles[r].clear();
            self.row_underline[r] = Some((u, end));
            if self.caret_on {
                let cur = pre.cursor.map_or(end, |c| {
                    let c = c.min(pre.text.len());
                    u + utf16_len(pre.text.get(..c).unwrap_or(&pre.text))
                });
                self.row_carets[r].push(cur);
            }
        }
    }

    /// Construit la carte des rangées (quelle partie de quelle ligne), sans le texte.
    fn build_layout(&mut self) {
        self.sync();
        let first = self.first_pos();
        let first = walker!(self).normalize(first);
        self.set_first_pos(first);
        let window = self.cols.max(40) * 4 + 64;
        let (rows, hscroll) = (self.rows, self.hscroll);
        let mut layout = std::mem::take(&mut self.layout);
        walker!(self).build(first, rows, hscroll, window, &mut layout);
        self.layout = layout;
    }

    /// Les rangées préparées par [`build_view`](Self::build_view).
    #[must_use]
    pub fn view(&self) -> ViewRows<'_> {
        ViewRows { editor: self }
    }

    // --- Repli de code -----------------------------------------------------------------------

    /// La rangée `row` (valide après `build_view`) est la fin d'une ligne repliée : son repère
    /// « … » est affiché à la suite du texte.
    #[must_use]
    pub fn row_is_folded(&self, row: usize) -> bool {
        self.layout.get(row).is_some_and(|r| r.folded)
    }

    /// La rangée `row` commence une ligne repliable ou repliée (flèche de la gouttière).
    #[must_use]
    pub fn row_fold_mark(&self, row: usize) -> FoldMark {
        self.layout.get(row).map_or(FoldMark::None, |r| r.fold)
    }

    /// Texte de la rangée sans le repère de repli (pour mesurer où il commence).
    #[must_use]
    pub fn row_plain_text(&self, row: usize) -> &str {
        let text = self.row_text(row);
        if self.row_is_folded(row) {
            text.strip_suffix(FOLD_MARKER).unwrap_or(text)
        } else {
            text
        }
    }

    /// Replie ou déplie la région de la ligne affichée à la rangée `row`. Renvoie `true` si
    /// quelque chose a changé.
    pub fn toggle_fold_row(&mut self, row: usize) -> bool {
        self.build_layout();
        let Some(info) = self.layout.get(row).copied() else {
            return false;
        };
        if info.line >= self.line_count() {
            return false;
        }
        self.toggle_fold_line(info.line)
    }

    fn toggle_fold_line(&mut self, line: usize) -> bool {
        let buf = self.doc.buffer();
        if self.folds.unfold_line(buf, line) {
            self.fold_dirty = true;
            self.sync();
            return true;
        }
        self.fold_line(line)
    }

    /// Replie la région qui commence à `line`, s'il y en a une. Les curseurs qu'elle masque
    /// passent à la fin de la ligne d'en-tête.
    fn fold_line(&mut self, line: usize) -> bool {
        let buf = self.doc.buffer();
        let Some(last) = fold::region_end(buf, line) else {
            return false;
        };
        self.folds.fold(buf, line, last);
        let end_of_header = buf.line_end(line);
        let (from, to) = (end_of_header + 1, buf.line_end(last));
        self.fold_dirty = true;
        self.sync();
        // curseurs masqués : on les ramène sur l'en-tête
        let mut moved = false;
        let sels: Vec<Selection> = self
            .doc
            .selections()
            .iter()
            .map(|s| {
                let hide = |p: usize| p >= from && p <= to;
                if hide(s.head) || hide(s.anchor) {
                    moved = true;
                    Selection::caret(end_of_header)
                } else {
                    *s
                }
            })
            .collect();
        if moved {
            let primary = self.doc.selections().primary_index();
            self.doc
                .set_selections(SelectionSet::from_vec(sels, primary));
        }
        true
    }

    /// Fold : replie la région du curseur (la ligne si elle est repliable, sinon la région qui
    /// l'englobe).
    fn fold_at_caret(&mut self) {
        let head = self.doc.selections().primary().head;
        let buf = self.doc.buffer();
        let line = buf.char_to_line(head);
        let header = if fold::foldable(buf, line) {
            Some(line)
        } else {
            // première ligne au-dessus moins indentée : l'en-tête de la région qui englobe
            fold::indent_of(buf, line).and_then(|indent| {
                (0..line)
                    .rev()
                    .take(5000)
                    .find(|&l| fold::indent_of(buf, l).is_some_and(|i| i < indent))
            })
        };
        if let Some(h) = header {
            self.fold_line(h);
        }
        self.ensure_caret_visible();
    }

    /// Unfold : déplie la région du curseur.
    fn unfold_at_caret(&mut self) {
        let head = self.doc.selections().primary().head;
        let buf = self.doc.buffer();
        let line = buf.char_to_line(head);
        if self.folds.unfold_line(buf, line) {
            self.fold_dirty = true;
            self.sync();
        }
    }

    /// Fold All : replie toutes les régions de premier niveau (lignes non indentées).
    fn fold_all(&mut self) {
        let buf = self.doc.buffer();
        let total = buf.len_lines().min(100_000);
        let mut regions = Vec::new();
        let mut line = 0;
        while line < total {
            if fold::indent_of(buf, line) == Some(0) {
                if let Some(last) = fold::region_end(buf, line) {
                    regions.push((line, last));
                    line = last + 1;
                    continue;
                }
            }
            line += 1;
        }
        self.folds.replace_all(buf, &regions);
        self.fold_dirty = true;
        self.sync();
        // les curseurs masqués remontent sur l'en-tête de leur région
        let mut sels = Vec::new();
        let mut moved = false;
        for s in self.doc.selections().iter() {
            let line = self.doc.buffer().char_to_line(s.head);
            let hidden = walker!(self).hidden_by(line);
            match hidden {
                Some(h) => {
                    moved = true;
                    sels.push(Selection::caret(self.doc.buffer().line_end(h)));
                }
                None => sels.push(*s),
            }
        }
        if moved {
            let primary = self.doc.selections().primary_index().min(sels.len() - 1);
            self.doc
                .set_selections(SelectionSet::from_vec(sels, primary));
        }
        self.ensure_caret_visible();
    }

    /// Unfold All : déplie tout.
    fn unfold_all(&mut self) {
        if self.folds.unfold_all() {
            self.fold_dirty = true;
            self.sync();
        }
    }

    /// Nombre de régions repliées.
    #[must_use]
    pub fn fold_count(&self) -> usize {
        self.folds.len()
    }

    /// Texte affiché de la rangée `row` (valide après `build_view`).
    #[must_use]
    pub fn row_text(&self, row: usize) -> &str {
        self.row_text.get(row).map_or("", String::as_str)
    }

    /// Colonne (en caractères) correspondant à une position UTF-16 dans la rangée `row`.
    #[must_use]
    pub fn char_col_of_utf16(&self, row: usize, utf16: usize) -> usize {
        let mut units = 0;
        for (i, c) in self.row_text(row).chars().enumerate() {
            if units >= utf16 {
                return i;
            }
            units += c.len_utf16();
        }
        self.row_text(row).chars().count()
    }

    /// Rangée et position UTF-16 du curseur principal (pour placer la fenêtre de l'IME).
    #[must_use]
    pub fn caret_anchor(&self) -> Option<(usize, usize)> {
        self.ime_anchor
    }
}

fn utf16_of_char_col(text: &str, col: usize) -> usize {
    text.chars().take(col).map(char::len_utf16).sum()
}

/// Position UTF-16 correspondant à un décalage en octets (sur une frontière de caractère).
fn utf16_of_byte(text: &str, byte: usize) -> usize {
    let mut units = 0;
    for (b, c) in text.char_indices() {
        if b >= byte {
            break;
        }
        units += c.len_utf16();
    }
    units
}

fn byte_of_utf16(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (b, c) in text.char_indices() {
        if units >= utf16 {
            return b;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// Rangées visibles, vues par le moteur de rendu.
#[derive(Debug)]
pub struct ViewRows<'a> {
    editor: &'a Editor,
}

impl RowSource for ViewRows<'_> {
    fn len(&self) -> usize {
        self.editor.row_text.len()
    }

    fn row(&self, index: usize) -> Row<'_> {
        let e = self.editor;
        let info = e.layout[index];
        let real = info.line < e.line_count();
        Row {
            // le numéro n'est affiché que sur le premier segment d'une ligne
            number: if real && info.first {
                info.line as u32 + 1
            } else {
                0
            },
            current: real && info.line == e.cursor_line,
            text: &e.row_text[index],
            selections: &e.row_sels[index],
            carets: &e.row_carets[index],
            underline: e.row_underline[index],
            spans: &e.row_spans[index],
            squiggles: &e.row_squiggles[index],
            fold: info.fold,
        }
    }
}

/// Lignes du document, vues par la minimap.
#[derive(Debug)]
pub struct BufferLines<'a>(&'a paulwrite_core::Buffer, &'a SpanCache);

impl MinimapSource for BufferLines<'_> {
    fn len(&self) -> usize {
        self.0.len_lines()
    }

    fn line(&self, index: usize, out: &mut String) {
        for chunk in self.0.line(index).chunks() {
            out.push_str(chunk);
        }
        while out.ends_with(['\n', '\r']) {
            out.pop();
        }
    }

    fn colors(&self, index: usize, text: &str, out: &mut Vec<(usize, usize, u8)>) {
        if self.1.is_empty() || index >= self.0.len_lines() {
            return;
        }
        let bs = self.0.rope().line_to_byte(index);
        let be = bs + text.len();
        // octets du document -> caractères de la ligne, en un seul passage (plages triées)
        let bytes = text.as_bytes();
        let (mut chars, mut at) = (0usize, 0usize);
        let mut to_char = |target: usize| {
            while at < target.min(bytes.len()) {
                at += match bytes[at] {
                    0..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                chars += 1;
            }
            chars
        };
        for sp in self.1.overlapping(bs, be) {
            let (a, b) = (sp.start.max(bs) - bs, sp.end.min(be) - bs);
            if a < b {
                let (ca, cb) = (to_char(a), to_char(b));
                out.push((ca, cb, sp.kind));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut e = Editor::from_document(Document::from_text(text), None);
        e.set_viewport_rows(5);
        e
    }

    fn view_texts(e: &mut Editor) -> Vec<String> {
        e.build_view();
        let v = e.view();
        (0..v.len()).map(|i| v.row(i).text.to_owned()).collect()
    }

    #[test]
    fn frappe_et_annulation() {
        let mut e = editor("");
        for (i, c) in "abc".chars().enumerate() {
            e.insert_text(&c.to_string(), i as u64);
        }
        assert_eq!(e.document().buffer().rope().to_string(), "abc");
        assert!(e.is_modified());
        e.execute(Command::Undo, 100);
        assert_eq!(e.document().buffer().rope().to_string(), "");
        e.execute(Command::Redo, 101);
        assert_eq!(e.document().buffer().rope().to_string(), "abc");
        assert_eq!(e.title(), "untitled* - PaulWrite");
        e.mark_saved(e.document().buffer().revision());
        assert!(!e.is_modified());
    }

    #[test]
    fn rangees_sans_fin_de_ligne_et_completees_par_des_vides() {
        let mut e = editor("un\r\ndeux\ntrois");
        assert_eq!(view_texts(&mut e), ["un", "deux", "trois", "", ""]);
    }

    #[test]
    fn le_curseur_est_dans_la_bonne_rangee_et_clignote() {
        let mut e = editor("abc\ndef");
        e.execute(Command::Move(Motion::Down), 0);
        e.execute(Command::Move(Motion::Right), 0);
        e.build_view();
        assert!(e.view().row(0).carets.is_empty());
        assert_eq!(e.view().row(1).carets, [1]);
        e.set_caret_visible(false);
        e.build_view();
        assert!(e.view().row(1).carets.is_empty());
    }

    #[test]
    fn selection_sur_plusieurs_lignes_avec_saut_de_ligne() {
        let mut e = editor("abc\ndef\nghi");
        e.execute(Command::Move(Motion::Right), 0);
        e.execute(Command::Extend(Motion::Down), 0);
        e.execute(Command::Extend(Motion::Down), 0);
        e.build_view();
        let v = e.view();
        // ligne 0 : de la colonne 1 à la fin, saut de ligne compris (3 unités + 1)
        assert_eq!(v.row(0).selections, [(1, 4)]);
        assert_eq!(v.row(1).selections, [(0, 4)]);
        // ligne 2 : de 0 à la colonne 1
        assert_eq!(v.row(2).selections, [(0, 1)]);
    }

    #[test]
    fn positions_utf16_avec_emoji() {
        let mut e = editor("a😀b");
        e.execute(Command::Move(Motion::Right), 0);
        e.execute(Command::Move(Motion::Right), 0);
        e.build_view();
        // curseur après « a😀 » : 1 + 2 unités UTF-16
        assert_eq!(e.view().row(0).carets, [3]);
        assert_eq!(e.char_col_of_utf16(0, 3), 2);
        assert_eq!(e.char_col_of_utf16(0, 1), 1);
    }

    #[test]
    fn la_vue_suit_le_curseur_et_le_defilement_est_borne() {
        let text: String = (0..100).map(|i| format!("ligne {i}\n")).collect();
        let mut e = editor(&text);
        assert_eq!(e.first_line(), 0);
        e.execute(Command::Move(Motion::DocEnd), 0);
        let last = e.document().buffer().len_lines() - 1;
        assert_eq!(e.first_line(), last + 1 - 5);
        e.execute(Command::Move(Motion::DocStart), 0);
        assert_eq!(e.first_line(), 0);
        e.scroll_by(-10);
        assert_eq!(e.first_line(), 0);
        e.scroll_by(10_000);
        assert_eq!(e.first_line(), last);
        assert_eq!(e.scrolled_since_draw(), last as isize);
        e.mark_drawn();
        assert_eq!(e.scrolled_since_draw(), 0);
    }

    #[test]
    fn page_suivante_et_precedente() {
        let text: String = (0..100).map(|i| format!("ligne {i}\n")).collect();
        let mut e = editor(&text);
        e.execute(Command::PageDown(false), 0);
        assert_eq!(
            e.document()
                .buffer()
                .char_to_line(e.document().selections().primary().head),
            4
        );
        assert_eq!(e.first_line(), 4);
        e.execute(Command::PageUp(false), 0);
        assert_eq!(e.first_line(), 0);
        assert_eq!(e.document().selections().primary().head, 0);
    }

    #[test]
    fn copier_couper_coller() {
        let mut e = editor("hello world");
        assert_eq!(e.execute(Command::Copy, 0), None, "rien de sélectionné");
        e.execute(Command::Extend(Motion::WordRight), 0);
        assert_eq!(
            e.execute(Command::Copy, 0),
            Some(Effect::SetClipboard("hello ".into()))
        );
        assert_eq!(
            e.execute(Command::Cut, 1),
            Some(Effect::SetClipboard("hello ".into()))
        );
        assert_eq!(e.document().buffer().rope().to_string(), "world");
        assert_eq!(e.execute(Command::Paste, 2), Some(Effect::RequestPaste));
        e.insert_text("a\r\nb", 3);
        assert_eq!(e.document().buffer().rope().to_string(), "a\nbworld");
    }

    #[test]
    fn le_collage_adopte_les_fins_de_ligne_du_document() {
        let doc = Document::from_buffer(
            paulwrite_core::Buffer::from_text("x"),
            paulwrite_core::Encoding::UTF8,
            paulwrite_core::LineEnding::CrLf,
        );
        let mut e = Editor::from_document(doc, None);
        e.insert_text("a\nb\rc", 0);
        assert_eq!(e.document().buffer().rope().to_string(), "a\r\nb\r\ncx");
    }

    #[test]
    fn multi_curseurs() {
        let mut e = editor("ab\ncd\nef");
        e.execute(Command::AddCursorBelow, 0);
        e.execute(Command::AddCursorBelow, 0);
        assert_eq!(e.document().selections().len(), 3);
        e.insert_text("X", 0);
        assert_eq!(e.document().buffer().rope().to_string(), "Xab\nXcd\nXef");
        e.execute(Command::CollapseSelections, 1);
        assert_eq!(e.document().selections().len(), 1);
        // en haut du document : pas de curseur supplémentaire
        e.execute(Command::Move(Motion::DocStart), 2);
        e.execute(Command::AddCursorAbove, 2);
        assert_eq!(e.document().selections().len(), 1);
    }

    #[test]
    fn clic_et_glissement() {
        let mut e = editor("abcdef\nghijkl");
        e.click(1, 3, false);
        assert_eq!(e.document().selections().primary().head, 10);
        e.click(0, 2, true);
        let s = e.document().selections().primary();
        assert_eq!((s.anchor, s.head), (10, 2));
        // clic au-delà de la fin de ligne et du document
        e.click(0, 99, false);
        assert_eq!(e.document().selections().primary().head, 6);
        e.click(4, 0, false);
        assert_eq!(e.document().selections().primary().head, 7);
    }

    #[test]
    fn composition_ime_affichee_et_soulignee() {
        let mut e = editor("ab");
        e.execute(Command::Move(Motion::Right), 0);
        e.set_preedit(Some(Preedit {
            text: "かな".into(),
            cursor: Some("か".len()),
        }));
        e.build_view();
        let v = e.view();
        let r = v.row(0);
        assert_eq!(r.text, "aかなb");
        assert_eq!(r.underline, Some((1, 3)));
        assert_eq!(r.carets, [2]);
        e.set_preedit(None);
        e.build_view();
        assert_eq!(e.view().row(0).text, "ab");
        assert_eq!(e.view().row(0).underline, None);
    }

    #[test]
    fn contexte_pour_les_plugins() {
        let mut e = editor("abc\ndéf\nghi");
        assert_eq!(e.caret_line_col(), (1, 1));
        assert_eq!(e.selection_string(100), "");
        e.execute(Command::Move(Motion::Down), 0);
        e.execute(Command::Move(Motion::Right), 0);
        e.execute(Command::Extend(Motion::Right), 0);
        assert_eq!(e.caret_line_col(), (2, 3));
        assert_eq!(e.selection_string(100), "é");
        // troncature sur une frontière de caractère : « é » fait 2 octets
        assert_eq!(e.selection_string(1), "");
        assert_eq!(
            e.execute(Command::ReloadPlugins, 0),
            Some(Effect::ReloadPlugins)
        );
    }

    #[test]
    fn diagnostics_en_soulignements_par_rangee() {
        // « é😀 » compte pour 3 unités UTF-16 : la colonne du serveur est bien celle de la rangée
        let mut e = editor("é😀 faute\nsuite\nfin");
        e.set_squiggles(vec![
            Squiggle {
                start: (0, 4),
                end: (0, 9),
                severity: 0,
            },
            // plage sur plusieurs lignes : du milieu de la ligne 0 au milieu de la ligne 2
            Squiggle {
                start: (0, 6),
                end: (2, 2),
                severity: 1,
            },
        ]);
        e.build_view();
        let v = e.view();
        assert_eq!(v.row(0).squiggles, [(4, 9, 0), (6, 9, 1)]);
        assert_eq!(v.row(1).squiggles, [(0, 5, 1)], "ligne entière");
        assert_eq!(v.row(2).squiggles, [(0, 2, 1)]);
        // hors du texte : bornes ramenées dans la ligne, sans panique
        e.set_squiggles(vec![Squiggle {
            start: (1, 0),
            end: (1, 99),
            severity: 2,
        }]);
        e.build_view();
        assert_eq!(e.view().row(1).squiggles, [(0, 5, 2)]);
        e.enable_lsp(false);
        assert!(e.squiggles().is_empty());
    }

    #[test]
    fn contexte_de_completion() {
        let mut e = editor("let valeur_ab = 1;");
        for _ in 0..13 {
            e.execute(Command::Move(Motion::Right), 0);
        }
        // curseur après « valeur_ab » : 9 caractères d'identifiant avant lui
        assert_eq!(e.word_prefix_chars(), 9);
        assert_eq!(e.caret_lsp_position(), (0, 13));
        e.complete(9, "valeur_abc()", 0);
        assert_eq!(
            e.document().buffer().rope().to_string(),
            "let valeur_abc() = 1;"
        );
        // en début de ligne : aucun préfixe
        e.execute(Command::Move(Motion::LineStart), 0);
        assert_eq!(e.word_prefix_chars(), 0);
        // suivi des modifications au format LSP
        e.enable_lsp(true);
        e.insert_text("é", 1);
        let changes = e.take_lsp_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].text, "é");
        assert!(e.take_lsp_changes().is_empty());
    }

    #[test]
    fn aller_a_une_ligne_centre_la_vue() {
        let text: String = (0..200).map(|i| format!("ligne {i}\n")).collect();
        let mut e = editor(&text);
        e.set_viewport_rows(11);
        e.goto(100, 3);
        let head = e.document().selections().primary().head;
        assert_eq!(e.document().buffer().char_to_line_col(head), (99, 3));
        // centrée : la ligne est au milieu des 11 rangées
        assert_eq!(e.first_line(), 99 - 5);
        // hors du document : ramené à la dernière ligne (vide après le dernier saut de ligne)
        e.goto(100_000, 0);
        let head = e.document().selections().primary().head;
        assert_eq!(e.document().buffer().char_to_line(head), e.line_count() - 1);
        // colonne trop grande : fin de ligne ; ligne 0 : première ligne
        e.goto(0, 999);
        let head = e.document().selections().primary().head;
        assert_eq!(
            e.document().buffer().char_to_line_col(head),
            (0, "ligne 0".len())
        );
        assert_eq!(e.first_line(), 0);
    }

    #[test]
    fn les_commandes_du_panneau_ouvrent_le_bon_mode() {
        let mut e = editor("a");
        assert_eq!(
            e.execute(Command::GotoFile, 0),
            Some(Effect::OpenPalette(PaletteMode::Files))
        );
        assert_eq!(
            e.execute(Command::CommandPalette, 0),
            Some(Effect::OpenPalette(PaletteMode::Commands))
        );
        assert_eq!(
            e.execute(Command::GotoLine, 0),
            Some(Effect::OpenPalette(PaletteMode::GotoLine))
        );
        assert_eq!(
            e.execute(Command::SearchProject, 0),
            Some(Effect::OpenPalette(PaletteMode::Search))
        );
    }

    fn status(e: &Editor) -> String {
        let mut s = String::new();
        e.write_status(&mut s);
        s
    }

    #[test]
    fn texte_de_la_barre_d_etat() {
        let mut e = editor(
            "ab
	cdef
z",
        );
        assert_eq!(status(&e), "Line 1, Column 1");
        e.click(1, 1, false);
        // la tabulation compte pour 4 colonnes, comme à l'écran
        assert_eq!(status(&e), "Line 2, Column 5");
        // sélection sur une ligne, sur plusieurs lignes, plusieurs curseurs
        e.click(1, 1, false);
        e.click(1, 4, true);
        assert_eq!(status(&e), "3 characters selected");
        e.click(0, 1, false);
        e.click(1, 2, true);
        assert_eq!(status(&e), "2 lines, 4 characters selected");
        e.click(0, 0, false);
        e.click(0, 1, true);
        assert_eq!(status(&e), "1 character selected");
        e.execute(Command::AddCursorBelow, 0);
        assert_eq!(status(&e), "2 selection regions");
    }

    #[test]
    fn gouttiere_numeros_et_ligne_courante() {
        let mut e = Editor::from_document(
            Document::from_text(
                &"x
"
                .repeat(120),
            ),
            None,
        );
        e.set_viewport_rows(5);
        assert_eq!(e.line_digits(), 3);
        e.click(2, 0, false);
        e.build_view();
        let v = e.view();
        let rows: Vec<_> = (0..v.len())
            .map(|i| (v.row(i).number, v.row(i).current))
            .collect();
        assert_eq!(
            rows,
            [(1, false), (2, false), (3, true), (4, false), (5, false)]
        );
        // en bas du document : pas de numéro au-delà de la dernière ligne
        e.scroll_by(1000);
        e.build_view();
        let v = e.view();
        assert_eq!(v.row(0).number, 121);
        assert_eq!(v.row(1).number, 0);
        assert_eq!(Editor::new().line_digits(), 1);
    }

    #[test]
    fn defilement_par_la_minimap_et_la_barre() {
        let mut e = Editor::from_document(
            Document::from_text(
                &"ligne
"
                .repeat(100),
            ),
            None,
        );
        e.set_viewport_rows(10);
        e.scroll_to_center(50);
        assert_eq!(e.first_line(), 45);
        e.scroll_to_fraction(0.0);
        assert_eq!(e.first_line(), 0);
        e.scroll_to_fraction(1.0);
        assert_eq!(e.first_line(), e.line_count() - 1);
        assert_eq!(e.scroll().total, e.line_count());
        // le curseur n'a pas bougé
        assert_eq!(status(&e), "Line 1, Column 1");
        // texte de la minimap : ligne sans fin de ligne
        let mut line = String::new();
        e.minimap().line(3, &mut line);
        assert_eq!(line, "ligne");
        assert_eq!(e.minimap().len(), e.line_count());
    }

    #[test]
    fn clic_dans_la_gouttiere_selectionne_la_ligne() {
        let mut e = editor(
            "un
deux
trois",
        );
        e.select_row(1);
        assert_eq!(
            e.selected_text().as_deref(),
            Some(
                "deux
"
            )
        );
        e.select_row(2);
        assert_eq!(e.selected_text().as_deref(), Some("trois"));
        // au-delà de la fin : dernière ligne
        e.select_row(4);
        assert_eq!(e.selected_text().as_deref(), Some("trois"));
    }

    #[test]
    fn titre_avec_nom_de_fichier() {
        let e = Editor::from_document(Document::new(), Some(PathBuf::from("dossier/notes.txt")));
        assert!(
            e.title().ends_with("notes.txt - PaulWrite"),
            "chemin complet comme Sublime Text"
        );
    }

    #[test]
    fn les_plages_colorees_deviennent_des_positions_utf16_par_rangee() {
        use paulwrite_syntax::{kind, Span};
        let mut e = editor("é fn\nlet x");
        e.enable_syntax(true);
        // octets : « é » = 2, espace = 1, « fn » = 3..5 de la première ligne ; ligne 2 commence à 6
        let span = |start, end, kind| Span { start, end, kind };
        let len = e.document().buffer().len_bytes();
        e.set_spans(
            0..len,
            vec![span(3, 5, kind::KEYWORD), span(6, 9, kind::KEYWORD)],
        );
        e.build_view();
        let v = e.view();
        // « é fn » : « fn » est aux colonnes 2..4 en UTF-16
        assert_eq!(v.row(0).spans, [(2, 4, kind::KEYWORD)]);
        assert_eq!(v.row(1).spans, [(0, 3, kind::KEYWORD)]);
    }

    #[test]
    fn les_couleurs_suivent_la_frappe_en_attendant_le_worker() {
        use paulwrite_syntax::{kind, Span};
        let mut e = editor("fn a");
        e.enable_syntax(true);
        e.set_spans(
            0..4,
            vec![Span {
                start: 0,
                end: 2,
                kind: kind::KEYWORD,
            }],
        );
        // on tape deux caractères au début : la plage « fn » est repoussée
        e.insert_text("xy", 0);
        let edits = e.take_edits();
        assert_eq!(edits.len(), 1);
        e.build_view();
        assert_eq!(e.view().row(0).text, "xyfn a");
        assert_eq!(e.view().row(0).spans, [(2, 4, kind::KEYWORD)]);
        // coloration désactivée : plus de plages, plus de journal
        e.enable_syntax(false);
        e.insert_text("z", 1);
        assert!(e.take_edits().is_empty());
        e.build_view();
        assert!(e.view().row(0).spans.is_empty());
    }

    #[test]
    fn fenetre_d_octets_des_lignes_visibles() {
        let text: String = (0..100).map(|i| format!("l{i}\n")).collect();
        let mut e = editor(&text);
        // 5 lignes visibles, première ligne 0 : octets des lignes 0..5 (+ marge)
        let v = e.view_bytes(0);
        assert_eq!(v, 0..e.document().buffer().rope().line_to_byte(5));
        e.scroll_by(50);
        let v = e.view_bytes(2);
        let rope = e.document().buffer().rope().clone();
        assert_eq!(v, rope.line_to_byte(48)..rope.line_to_byte(57));
        e.scroll_by(1000);
        // fin du document : bornée
        assert_eq!(e.view_bytes(100).end, rope.len_bytes());
        assert!(!e.spans_cover_view());
    }

    #[test]
    fn textes_en_francais() {
        let mut e = editor(
            "abc
def",
        );
        e.set_lang(Lang::Fr);
        assert_eq!(status(&e), "Ligne 1, colonne 1");
        e.execute(Command::SelectAll, 0);
        assert_eq!(status(&e), "2 lignes, 7 caractères sélectionnés");
        e.click(0, 1, false);
        e.click(0, 2, true);
        assert_eq!(status(&e), "1 caractère sélectionné");
        e.execute(Command::AddCursorBelow, 0);
        assert_eq!(status(&e), "2 zones de sélection");
        assert_eq!(Editor::new().display_name(), "untitled");
        let mut n = Editor::new();
        n.set_lang(Lang::Fr);
        assert_eq!(n.display_name(), "sans titre");
        assert_eq!(n.title(), "sans titre - PaulWrite");
        // la langue survit au chargement d'un autre document
        n.load(Document::from_text("x"), None);
        assert_eq!(n.display_name(), "sans titre");
    }

    #[test]
    fn la_minimap_recoit_les_couleurs_du_document_entier() {
        use paulwrite_syntax::{kind, Span};
        let mut e = editor(
            "é fn
let x",
        );
        e.enable_syntax(true);
        let len = e.document().buffer().len_bytes();
        let span = |start, end, kind| Span { start, end, kind };
        let colors = |e: &Editor, line: usize, text: &str| {
            let mut out = Vec::new();
            e.minimap().colors(line, text, &mut out);
            out
        };
        // sans coloration reçue : rien
        assert!(colors(&e, 0, "é fn").is_empty());
        let v0 = e.overview_version();
        // octets : « é » = 2, « fn » = 3..5 ; la ligne 2 commence à 6 (« let » = 6..9)
        e.set_overview(
            0..len,
            vec![span(3, 5, kind::KEYWORD), span(6, 9, kind::KEYWORD)],
        );
        assert!(e.overview_version() > v0);
        // positions en caractères (« é » compte pour un)
        assert_eq!(colors(&e, 0, "é fn"), [(2, 4, kind::KEYWORD)]);
        assert_eq!(colors(&e, 1, "let x"), [(0, 3, kind::KEYWORD)]);
        assert!(colors(&e, 9, "").is_empty(), "ligne inexistante");
        // la frappe décale les couleurs en attendant la prochaine coloration complète
        e.insert_text("ab", 1);
        let _ = e.take_edits();
        assert_eq!(colors(&e, 0, "abé fn"), [(4, 6, kind::KEYWORD)]);
        // coloration coupée : la minimap redevient grise
        e.enable_syntax(false);
        assert!(colors(&e, 1, "let x").is_empty());
    }

    // --- Retour à la ligne ---------------------------------------------------------------------

    fn wrapped(text: &str, cols: usize, rows: usize) -> Editor {
        let mut e = Editor::from_document(Document::from_text(text), None);
        e.set_viewport_rows(rows);
        e.set_wrap(true);
        e.set_viewport_cols(cols);
        e
    }

    fn numbers(e: &mut Editor) -> Vec<u32> {
        e.build_view();
        let v = e.view();
        (0..v.len()).map(|i| v.row(i).number).collect()
    }

    const WRAPPED: &str = "aaaa bbbb cccc\nxy\nzzzz zzzz";

    #[test]
    fn les_lignes_longues_se_coupent_en_rangees() {
        let mut e = wrapped(WRAPPED, 5, 8);
        assert_eq!(
            view_texts(&mut e),
            ["aaaa ", "bbbb ", "cccc", "xy", "zzzz ", "zzzz", "", ""]
        );
        // le numéro n'est affiché que sur le premier segment de chaque ligne
        assert_eq!(numbers(&mut e), [1, 0, 0, 2, 3, 0, 0, 0]);
        // sans retour à la ligne : une rangée par ligne, comme avant
        e.set_wrap(false);
        assert_eq!(
            view_texts(&mut e),
            ["aaaa bbbb cccc", "xy", "zzzz zzzz", "", "", "", "", ""]
        );
        assert_eq!(numbers(&mut e), [1, 2, 3, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn la_ligne_du_curseur_est_en_evidence_sur_tous_ses_segments() {
        let mut e = wrapped(WRAPPED, 5, 8);
        e.click(1, 1, false);
        e.build_view();
        let v = e.view();
        let current: Vec<bool> = (0..8).map(|i| v.row(i).current).collect();
        assert_eq!(
            current,
            [true, true, true, false, false, false, false, false]
        );
    }

    #[test]
    fn clic_dans_une_rangee_de_continuation() {
        let mut e = wrapped(WRAPPED, 5, 8);
        e.click(1, 2, false);
        assert_eq!(e.caret_line_col(), (1, 8));
        // loin à droite d'un segment coupé : on reste dans ce segment (pas au début du suivant)
        e.click(0, 99, false);
        assert_eq!(e.caret_line_col(), (1, 5));
        e.click(1, 99, false);
        assert_eq!(e.caret_line_col(), (1, 10));
        // dernier segment de la ligne : fin de ligne
        e.click(2, 99, false);
        assert_eq!(e.caret_line_col(), (1, 15));
        // rangée au-delà de la fin : fin du document
        e.click(7, 99, false);
        assert_eq!(e.caret_line_col(), (3, 10));
        // sélection au glisser, d'un segment à l'autre
        e.click(0, 1, false);
        e.click(2, 2, true);
        assert_eq!(e.selection_string(100), "aaa bbbb cc");
    }

    #[test]
    fn le_curseur_s_affiche_dans_la_bonne_rangee() {
        let mut e = wrapped(WRAPPED, 5, 8);
        e.click(1, 2, false);
        e.build_view();
        assert_eq!(e.view().row(1).carets, [2]);
        assert!(e.view().row(0).carets.is_empty());
        assert_eq!(e.caret_anchor(), Some((1, 2)));
        // au bord d'un segment, le curseur s'affiche au début du suivant
        e.execute(Command::Move(Motion::Left), 0);
        e.execute(Command::Move(Motion::Left), 0);
        e.build_view();
        assert_eq!(e.caret_line_col(), (1, 6));
        assert_eq!(e.view().row(1).carets, [0]);
        assert!(e.view().row(0).carets.is_empty());
        // fin d'une ligne : dernier segment, après le dernier caractère
        e.execute(Command::Move(Motion::LineEnd), 0);
        e.build_view();
        assert_eq!(e.view().row(2).carets, [4]);
    }

    #[test]
    fn haut_et_bas_suivent_les_rangees_affichees() {
        let mut e = wrapped(WRAPPED, 5, 8);
        e.click(1, 2, false); // rangée 1, colonne 2 : ligne 1, colonne 8
        let down = |e: &mut Editor| e.execute(Command::Move(Motion::Down), 0);
        let up = |e: &mut Editor| e.execute(Command::Move(Motion::Up), 0);
        down(&mut e);
        assert_eq!(e.caret_line_col(), (1, 13), "rangée « cccc »");
        down(&mut e);
        assert_eq!(
            e.caret_line_col(),
            (2, 3),
            "« xy » est plus court : fin de ligne"
        );
        down(&mut e);
        assert_eq!(
            e.caret_line_col(),
            (3, 3),
            "la colonne visée (2) est retrouvée"
        );
        up(&mut e);
        up(&mut e);
        assert_eq!(e.caret_line_col(), (1, 13));
        up(&mut e);
        up(&mut e);
        assert_eq!(e.caret_line_col(), (1, 3));
        // haut du document : le déplacement par ligne prend le relais (début de document)
        up(&mut e);
        assert_eq!(e.caret_line_col(), (1, 1));
        // la sélection s'étend rangée par rangée, et la colonne visée (2) est restée la même
        e.execute(Command::Extend(Motion::Down), 0);
        assert_eq!(e.selection_string(100), "aaaa bb");
    }

    #[test]
    fn defilement_par_rangees() {
        let mut e = wrapped(WRAPPED, 5, 3);
        assert_eq!(e.scroll().first, 0);
        e.mark_drawn();
        e.scroll_by(2);
        assert_eq!(view_texts(&mut e), ["cccc", "xy", "zzzz "]);
        // la ligne 1 est toujours la première affichée, mais sans numéro (continuation)
        assert_eq!(numbers(&mut e), [0, 2, 3]);
        assert_eq!(e.scroll().first, 0);
        assert_eq!(e.scrolled_since_draw(), 2, "deux rangées, pas deux lignes");
        e.scroll_by(1);
        assert_eq!(view_texts(&mut e), ["xy", "zzzz ", "zzzz"]);
        assert_eq!(e.scroll().first, 1);
        e.scroll_by(-2);
        assert_eq!(view_texts(&mut e), ["bbbb ", "cccc", "xy"]);
        assert_eq!(e.scrolled_since_draw(), 1);
        // jusqu'au bout : la dernière rangée reste affichée
        e.scroll_by(100);
        assert_eq!(view_texts(&mut e)[0], "zzzz");
        e.scroll_by(-100);
        assert_eq!(view_texts(&mut e)[0], "aaaa ");
    }

    #[test]
    fn le_curseur_reste_visible_avec_le_retour_a_la_ligne() {
        let mut e = wrapped(WRAPPED, 5, 3);
        e.execute(Command::Move(Motion::DocEnd), 0);
        assert_eq!(view_texts(&mut e), ["xy", "zzzz ", "zzzz"]);
        assert_eq!(e.view().row(2).carets, [4]);
        e.execute(Command::Move(Motion::DocStart), 0);
        assert_eq!(view_texts(&mut e), ["aaaa ", "bbbb ", "cccc"]);
        // descente rangée par rangée : la vue ne bouge qu'en bas
        for _ in 0..3 {
            e.execute(Command::Move(Motion::Down), 0);
        }
        assert_eq!(view_texts(&mut e), ["bbbb ", "cccc", "xy"]);
        assert_eq!(e.view().row(2).carets, [0]);
    }

    #[test]
    fn selection_a_travers_les_segments() {
        let mut e = wrapped(WRAPPED, 5, 8);
        e.execute(Command::SelectAll, 0);
        e.build_view();
        let v = e.view();
        assert_eq!(v.row(0).selections, [(0, 5)]);
        assert_eq!(v.row(1).selections, [(0, 5)]);
        // fin de ligne sélectionnée : la surbrillance déborde d'un caractère (dernier segment)
        assert_eq!(v.row(2).selections, [(0, 5)]);
        assert_eq!(v.row(3).selections, [(0, 3)]);
        assert_eq!(
            v.row(5).selections,
            [(0, 4)],
            "dernière ligne : pas de saut de ligne"
        );
    }

    #[test]
    fn diagnostics_et_couleurs_par_segment() {
        use paulwrite_syntax::Span;
        let mut e = wrapped("fn aaaa bbbb", 5, 4);
        e.enable_syntax(true);
        let span = |start, end, kind| Span { start, end, kind };
        e.set_spans(
            0..12,
            vec![span(0, 2, kind::KEYWORD), span(3, 7, kind::FUNCTION)],
        );
        e.set_squiggles(vec![Squiggle {
            start: (0, 5),
            end: (0, 10),
            severity: 0,
        }]);
        assert_eq!(view_texts(&mut e), ["fn ", "aaaa ", "bbbb", ""]);
        let v = e.view();
        assert_eq!(v.row(0).spans, [(0, 2, kind::KEYWORD)]);
        assert_eq!(v.row(1).spans, [(0, 4, kind::FUNCTION)]);
        assert!(v.row(2).spans.is_empty());
        // « aaaa bbbb » : le soulignement (colonnes 5..10) est coupé en deux rangées
        assert!(v.row(0).squiggles.is_empty());
        assert_eq!(v.row(1).squiggles, [(2, 5, 0)]);
        assert_eq!(v.row(2).squiggles, [(0, 2, 0)]);
    }

    #[test]
    fn la_largeur_change_la_disposition() {
        let mut e = wrapped("aaaa bbbb cccc", 5, 8);
        assert_eq!(view_texts(&mut e)[..3], ["aaaa ", "bbbb ", "cccc"]);
        e.set_viewport_cols(10);
        assert_eq!(view_texts(&mut e)[..2], ["aaaa bbbb ", "cccc"]);
        e.set_viewport_cols(40);
        assert_eq!(view_texts(&mut e)[0], "aaaa bbbb cccc");
        // la vue reste sur la ligne du curseur quand la largeur change
        let mut e = wrapped(&"mot ".repeat(40), 10, 2);
        e.execute(Command::Move(Motion::DocEnd), 0);
        e.set_viewport_cols(20);
        e.build_view();
        assert!(e.view().row(1).carets.len() + e.view().row(0).carets.len() == 1);
    }

    // --- Défilement horizontal -----------------------------------------------------------------

    #[test]
    fn sans_retour_a_la_ligne_la_vue_suit_le_curseur_horizontalement() {
        let line = "0123456789".repeat(10);
        let mut e = Editor::from_document(Document::from_text(&line), None);
        e.set_viewport_rows(3);
        e.set_viewport_cols(20);
        assert_eq!(e.hscroll(), 0);
        e.execute(Command::Move(Motion::LineEnd), 0);
        let h = e.hscroll();
        assert!(h > 70 && h < 100, "défilé jusqu'au curseur : {h}");
        e.build_view();
        let view = e.view();
        let row = view.row(0);
        assert!(row.text.starts_with(&line[h..h + 5]));
        assert_eq!(row.carets, [100 - h]);
        assert!(100 - h < 20, "le curseur est dans la zone affichée");
        // retour au début : la vue revient à gauche
        e.execute(Command::Move(Motion::LineStart), 0);
        assert_eq!(e.hscroll(), 0);
        // défilement à la molette
        e.scroll_horizontally(5);
        assert_eq!(e.hscroll(), 5);
        assert!(view_texts(&mut e)[0].starts_with("56789"));
        e.scroll_horizontally(-100);
        assert_eq!(e.hscroll(), 0);
        // avec le retour à la ligne : jamais de défilement horizontal
        e.set_wrap(true);
        e.scroll_horizontally(5);
        assert_eq!(e.hscroll(), 0);
    }

    #[test]
    fn une_ligne_geante_n_affiche_qu_une_fenetre() {
        let line = "x".repeat(2_000_000);
        let mut e = Editor::from_document(Document::from_text(&line), None);
        e.set_viewport_rows(2);
        e.set_viewport_cols(100);
        let t = view_texts(&mut e);
        assert!(t[0].len() < 1000, "fenêtre de {} caractères", t[0].len());
        e.execute(Command::Move(Motion::LineEnd), 0);
        assert!(e.hscroll() > 1_999_000);
        e.build_view();
        assert_eq!(e.view().row(0).carets.len(), 1);
    }

    // --- Repli de code -------------------------------------------------------------------------

    const CODE: &str = "fn a() {\n    x();\n    y();\n}\nz\n";

    fn folded() -> Editor {
        let mut e = editor(CODE);
        e.set_viewport_rows(6);
        e.execute(Command::Fold, 0);
        e
    }

    #[test]
    fn replier_masque_les_lignes_et_affiche_le_repere() {
        let mut e = folded();
        assert_eq!(e.fold_count(), 1);
        assert_eq!(
            view_texts(&mut e),
            ["fn a() {\u{2026}", "}", "z", "", "", ""]
        );
        assert_eq!(numbers(&mut e), [1, 4, 5, 6, 0, 0]);
        let v = e.view();
        // le repère « … » est en couleur de commentaire
        assert_eq!(v.row(0).spans, [(8, 9, kind::COMMENT)]);
        assert_eq!(v.row(0).fold, FoldMark::Closed);
        assert_eq!(v.row(1).fold, FoldMark::None);
        assert!(e.row_is_folded(0));
        assert_eq!(e.row_plain_text(0), "fn a() {");
    }

    #[test]
    fn la_fleche_de_repli_des_lignes_repliables() {
        let mut e = editor(CODE);
        // sans survol de la gouttière, la recherche des lignes repliables est évitée
        e.build_view();
        assert_eq!(e.view().row(0).fold, FoldMark::None);
        e.set_fold_marks(true);
        e.build_view();
        assert_eq!(e.view().row(0).fold, FoldMark::Open);
        assert_eq!(
            e.view().row(1).fold,
            FoldMark::None,
            "x(); n'est pas repliable"
        );
        assert_eq!(e.row_fold_mark(0), FoldMark::Open);
    }

    #[test]
    fn haut_et_bas_sautent_les_lignes_repliees() {
        let mut e = folded();
        e.execute(Command::Move(Motion::Down), 0);
        assert_eq!(
            e.caret_line_col(),
            (4, 1),
            "accolade fermante : les lignes 2 et 3 sont sautées"
        );
        e.execute(Command::Move(Motion::Up), 0);
        assert_eq!(e.caret_line_col(), (1, 1));
        assert_eq!(e.fold_count(), 1, "la région reste repliée");
    }

    #[test]
    fn le_curseur_replie_remonte_sur_l_en_tete() {
        let mut e = editor(CODE);
        e.set_viewport_rows(6);
        e.goto(3, 2); // dans « y(); »
        e.execute(Command::Fold, 0);
        // la région englobante est repliée, le curseur est ramené sur sa première ligne
        assert_eq!(e.fold_count(), 1);
        assert_eq!(e.caret_line_col(), (1, 9));
    }

    #[test]
    fn entrer_dans_une_region_repliee_la_deplie() {
        let mut e = folded();
        e.goto(2, 0);
        assert_eq!(e.fold_count(), 0);
        assert_eq!(view_texts(&mut e)[1], "    x();");
        // Unfold All
        let mut e = folded();
        e.execute(Command::UnfoldAll, 0);
        assert_eq!(e.fold_count(), 0);
        assert_eq!(view_texts(&mut e)[1], "    x();");
        // Unfold à partir de l'en-tête
        let mut e = folded();
        e.execute(Command::Unfold, 0);
        assert_eq!(e.fold_count(), 0);
    }

    #[test]
    fn la_frappe_suit_les_regions_repliees() {
        let mut e = folded();
        // frappe à la fin de l'en-tête : la région reste repliée
        e.execute(Command::Move(Motion::LineEnd), 0);
        e.insert_text("q", 1);
        assert_eq!(e.fold_count(), 1);
        assert_eq!(view_texts(&mut e)[0], "fn a() {q\u{2026}");
        // une ligne ajoutée avant décale la région sans la déplier
        e.execute(Command::Move(Motion::DocStart), 0);
        e.execute(Command::InsertNewline, 2);
        assert_eq!(e.fold_count(), 1);
        assert_eq!(view_texts(&mut e)[..3], ["", "fn a() {q\u{2026}", "}"]);
        // effacer le texte masqué (tout sélectionner) : plus de région
        e.execute(Command::SelectAll, 0);
        e.execute(Command::DeleteBackward, 3);
        assert_eq!(e.fold_count(), 0);
    }

    #[test]
    fn tout_replier_et_tout_deplier() {
        let text = "fn a() {\n    x();\n}\nfn b() {\n    y();\n    z();\n}\nlet c = 1;\n";
        let mut e = editor(text);
        e.set_viewport_rows(10);
        e.execute(Command::FoldAll, 0);
        assert_eq!(e.fold_count(), 2);
        assert_eq!(
            view_texts(&mut e)[..5],
            [
                "fn a() {\u{2026}",
                "}",
                "fn b() {\u{2026}",
                "}",
                "let c = 1;"
            ]
        );
        e.execute(Command::UnfoldAll, 0);
        assert_eq!(e.fold_count(), 0);
        assert_eq!(view_texts(&mut e)[1], "    x();");
    }

    #[test]
    fn clic_sur_la_fleche_et_sur_le_repere() {
        let mut e = editor(CODE);
        e.set_viewport_rows(6);
        e.build_view();
        assert!(e.toggle_fold_row(0), "replie");
        assert_eq!(e.fold_count(), 1);
        assert!(e.toggle_fold_row(0), "déplie");
        assert_eq!(e.fold_count(), 0);
        assert!(!e.toggle_fold_row(1), "x(); n'est pas repliable");
        assert!(!e.toggle_fold_row(99), "rangée inexistante");
        // plier une ligne masque ce que l'on cliquait : la rangée suivante est celle d'après
        e.toggle_fold_row(0);
        e.click(1, 0, false);
        assert_eq!(e.caret_line_col(), (4, 1));
    }

    #[test]
    fn defilement_et_barre_de_defilement_avec_des_lignes_repliees() {
        let mut e = folded();
        e.set_viewport_rows(2);
        e.scroll_by(1);
        assert_eq!(view_texts(&mut e), ["}", "z"]);
        e.scroll_by(-1);
        assert_eq!(view_texts(&mut e)[0], "fn a() {\u{2026}");
        // la barre de défilement compte les lignes affichées
        let s = e.scroll();
        assert_eq!((s.first, s.visible, s.total), (0, 2, 6));
        // une position placée sur une ligne masquée revient à l'en-tête
        e.set_first_line(2);
        assert_eq!(view_texts(&mut e)[0], "fn a() {\u{2026}");
        // le curseur en bas de la vue : la vue suit en sautant les lignes masquées
        e.set_viewport_rows(2);
        e.execute(Command::Move(Motion::DocEnd), 0);
        assert_eq!(view_texts(&mut e), ["z", ""]);
    }

    #[test]
    fn replier_avec_retour_a_la_ligne() {
        let text = "fn aaaa bbbb cccc {\n    x();\n}\n";
        let mut e = wrapped(text, 8, 8);
        e.execute(Command::Fold, 0);
        // le repère « … » est à la fin du **dernier** segment de l'en-tête
        let t = view_texts(&mut e);
        assert_eq!(t[..4], ["fn aaaa ", "bbbb ", "cccc {\u{2026}", "}"]);
        assert_eq!(e.view().row(0).fold, FoldMark::Closed);
        assert_eq!(e.view().row(1).fold, FoldMark::None);
        assert!(!e.row_is_folded(1) && e.row_is_folded(2));
    }

    // --- Édition au clavier --------------------------------------------------------------------

    fn run(e: &mut Editor, cmds: &[Command]) {
        for (i, c) in cmds.iter().enumerate() {
            e.execute(*c, 10_000 * i as u64);
        }
    }

    fn contenu(e: &Editor) -> String {
        e.document().buffer().to_string()
    }

    #[test]
    fn entree_reprend_l_indentation_et_les_paires_se_ferment() {
        let mut e = editor("");
        e.insert_text("fn a", 0);
        e.insert_text("(", 1);
        assert_eq!(contenu(&e), "fn a()", "paire automatique");
        e.insert_text(")", 2);
        e.insert_text(" ", 3);
        e.insert_text("{", 4);
        assert_eq!(contenu(&e), "fn a() {}");
        run(&mut e, &[Command::InsertNewline]);
        // la fermeture passe sur sa ligne, le curseur est sur la ligne du milieu, indenté
        assert_eq!(contenu(&e), "fn a() {\n    \n}");
        assert_eq!(e.caret_line_col(), (2, 5));
        e.insert_text("x", 5);
        assert_eq!(contenu(&e), "fn a() {\n    x\n}");
        // couper les paires automatiques
        let mut e = editor("");
        e.set_autopair(false);
        e.insert_text("(", 0);
        assert_eq!(contenu(&e), "(");
    }

    #[test]
    fn le_retour_arriere_enleve_la_paire_vide() {
        let mut e = editor("");
        e.insert_text("[", 0);
        assert_eq!(contenu(&e), "[]");
        run(&mut e, &[Command::DeleteBackward]);
        assert_eq!(contenu(&e), "");
        // hors paire : suppression ordinaire d'un caractère
        e.insert_text("ab", 10_000);
        run(&mut e, &[Command::DeleteBackward]);
        assert_eq!(contenu(&e), "a");
    }

    #[test]
    fn commandes_de_lignes_et_de_commentaires() {
        let mut e = editor("a\nb\nc\n");
        e.set_comment_style(CommentStyle::HASH);
        e.click(1, 0, false);
        run(&mut e, &[Command::DuplicateLine]);
        assert_eq!(contenu(&e), "a\nb\nb\nc\n");
        run(&mut e, &[Command::MoveLineUp]);
        assert_eq!(contenu(&e), "a\nb\nb\nc\n");
        run(&mut e, &[Command::MoveLineDown, Command::MoveLineDown]);
        assert_eq!(contenu(&e), "a\nb\nc\nb\n");
        run(&mut e, &[Command::ToggleComment]);
        assert_eq!(contenu(&e), "a\nb\nc\n# b\n");
        run(&mut e, &[Command::ToggleComment, Command::DeleteLine]);
        assert_eq!(contenu(&e), "a\nb\nc\n");
        // tout s'annule pas à pas (sept opérations)
        for _ in 0..7 {
            e.execute(Command::Undo, 0);
        }
        assert_eq!(contenu(&e), "a\nb\nc\n");
        // un langage sans commentaire : rien ne change
        let mut e = editor("x\n");
        e.set_comment_style(CommentStyle::default());
        run(&mut e, &[Command::ToggleComment]);
        assert_eq!(contenu(&e), "x\n");
    }

    #[test]
    fn indentation_et_selection_d_occurrences() {
        let mut e = editor("foo\nfoo\n");
        run(
            &mut e,
            &[Command::SelectNextOccurrence, Command::SelectNextOccurrence],
        );
        assert_eq!(
            e.selection_string(100),
            "foo\nfoo",
            "deux occurrences sélectionnées"
        );
        e.insert_text("bar", 0);
        assert_eq!(contenu(&e), "bar\nbar\n", "frappe sur les deux curseurs");
        // Maj+Tab désindente, Tab indente
        let mut e = editor("a\nb\n");
        run(
            &mut e,
            &[Command::SelectLine, Command::SelectLine, Command::InsertTab],
        );
        assert_eq!(contenu(&e), "    a\n    b\n");
        run(&mut e, &[Command::Unindent]);
        assert_eq!(contenu(&e), "a\nb\n");
    }
}
