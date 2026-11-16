//! État d'édition d'une fenêtre : document, vue (première ligne visible), exécution des
//! commandes et construction des rangées à dessiner. Aucune dépendance à winit ni à Win32 :
//! entièrement testable.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use paulwrite_core::motion;
use paulwrite_core::{Document, Motion, Selection, SelectionSet};
use paulwrite_render::{Row, RowSource};

use crate::command::{Command, Effect};

/// Texte de composition IME en cours (non encore validé).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// Position du curseur dans `text`, en octets.
    pub cursor: Option<usize>,
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
    first_line: usize,
    /// Nombre de rangées entières visibles.
    rows: usize,
    drawn_first_line: usize,
    caret_on: bool,
    preedit: Option<Preedit>,
    // Rangées construites par `build_view` (tampons réutilisés d'une image à l'autre).
    row_text: Vec<String>,
    row_sels: Vec<Vec<(usize, usize)>>,
    row_carets: Vec<Vec<usize>>,
    row_underline: Vec<Option<(usize, usize)>>,
    ime_anchor: Option<(usize, usize)>,
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
        Self {
            doc,
            path,
            saved_revision,
            first_line: 0,
            rows: 40,
            drawn_first_line: 0,
            caret_on: true,
            preedit: None,
            row_text: Vec::new(),
            row_sels: Vec::new(),
            row_carets: Vec::new(),
            row_underline: Vec::new(),
            ime_anchor: None,
        }
    }

    /// Remplace le document (fichier chargé). La vue repart du haut.
    pub fn load(&mut self, doc: Document, path: Option<PathBuf>) {
        let rows = self.rows;
        *self = Self::from_document(doc, path);
        self.rows = rows;
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
            || "Sans titre".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Titre de la fenêtre.
    #[must_use]
    pub fn title(&self) -> String {
        let star = if self.is_modified() { "*" } else { "" };
        format!("{}{star} — PaulWrite", self.display_name())
    }

    // --- Vue ---------------------------------------------------------------------------------

    /// Fixe le nombre de rangées entières visibles.
    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
        self.clamp_first_line();
        self.ensure_caret_visible();
    }

    fn max_first_line(&self) -> usize {
        self.doc.buffer().len_lines().saturating_sub(1)
    }

    fn clamp_first_line(&mut self) {
        self.first_line = self.first_line.min(self.max_first_line());
    }

    /// Fait défiler la vue de `lines` lignes (positif : vers le bas du document).
    pub fn scroll_by(&mut self, lines: isize) {
        let target = self.first_line as isize + lines;
        self.first_line = (target.max(0) as usize).min(self.max_first_line());
    }

    /// Déplace la vue pour que le curseur principal soit visible.
    pub fn ensure_caret_visible(&mut self) {
        let head = self.doc.selections().primary().head;
        let line = self.doc.buffer().char_to_line(head);
        if line < self.first_line {
            self.first_line = line;
        } else if line >= self.first_line + self.rows {
            self.first_line = line + 1 - self.rows;
        }
    }

    /// De combien de rangées la vue a défilé depuis la dernière image dessinée.
    #[must_use]
    pub fn scrolled_since_draw(&self) -> isize {
        self.first_line as isize - self.drawn_first_line as isize
    }

    /// À appeler après avoir dessiné une image.
    pub fn mark_drawn(&mut self) {
        self.drawn_first_line = self.first_line;
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
            Command::Move(m) => {
                self.doc.move_selections(m, false);
                self.ensure_caret_visible();
            }
            Command::Extend(m) => {
                self.doc.move_selections(m, true);
                self.ensure_caret_visible();
            }
            Command::PageUp(extend) => self.page(false, extend),
            Command::PageDown(extend) => self.page(true, extend),
            Command::ScrollUp => self.scroll_by(-1),
            Command::ScrollDown => self.scroll_by(1),
            Command::InsertNewline => {
                self.doc.insert_newline(now_ms);
                self.ensure_caret_visible();
            }
            Command::InsertTab => self.insert_text("    ", now_ms),
            Command::DeleteBackward => {
                self.doc.delete_backward(now_ms);
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
            Command::Quit => return Some(Effect::Quit),
        }
        None
    }

    /// Insère du texte à chaque curseur (frappe, validation IME, collage).
    pub fn insert_text(&mut self, text: &str, now_ms: u64) {
        self.caret_on = true;
        let text = normalize_newlines(text, self.doc.line_ending().as_str());
        if self.doc.insert_text(&text, now_ms) {
            self.ensure_caret_visible();
        }
    }

    fn page(&mut self, down: bool, extend: bool) {
        let n = self.rows.saturating_sub(1).max(1);
        let motion = if down { Motion::Down } else { Motion::Up };
        for _ in 0..n {
            self.doc.move_selections(motion, extend);
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
        let buf = self.doc.buffer();
        let line = (self.first_line + row).min(buf.len_lines().saturating_sub(1));
        let pos = buf.line_start(line) + char_col.min(buf.line_len(line));
        let anchor = if extend {
            self.doc.selections().primary().anchor
        } else {
            pos
        };
        self.doc
            .set_selections(SelectionSet::single(Selection::new(anchor, pos)));
        self.caret_on = true;
    }

    // --- Construction des rangées ------------------------------------------------------------

    /// Prépare les rangées visibles. À appeler avant de dessiner.
    pub fn build_view(&mut self) {
        let rows = self.rows;
        self.row_text.resize_with(rows, String::new);
        self.row_sels.resize_with(rows, Vec::new);
        self.row_carets.resize_with(rows, Vec::new);
        self.row_underline.resize(rows, None);

        let buf = self.doc.buffer();
        let total = buf.len_lines();
        let first = self.first_line;

        for r in 0..rows {
            let s = &mut self.row_text[r];
            s.clear();
            self.row_sels[r].clear();
            self.row_carets[r].clear();
            self.row_underline[r] = None;
            let line = first + r;
            if line < total {
                for chunk in buf.line(line).chunks() {
                    s.push_str(chunk);
                }
                while s.ends_with(['\n', '\r']) {
                    s.pop();
                }
            }
        }

        // Décorations : sélections et curseurs.
        let last_line = (first + rows).min(total).saturating_sub(1);
        let view_start = if first < total {
            buf.line_start(first)
        } else {
            buf.len_chars()
        };
        let view_end = if first < total {
            buf.line_end(last_line) + 1
        } else {
            buf.len_chars()
        };
        self.ime_anchor = None;
        let primary = self.doc.selections().primary_index();

        for (i, sel) in self.doc.selections().iter().enumerate() {
            if sel.end() < view_start || sel.start() > view_end || first >= total {
                continue;
            }
            let head_line = buf.char_to_line(sel.head);
            let l0 = buf.char_to_line(sel.start()).max(first);
            let l1 = buf.char_to_line(sel.end()).min(last_line);
            for l in l0..=l1 {
                let r = l - first;
                let ls = buf.line_start(l);
                let lchars = buf.line_len(l);
                let text = &self.row_text[r];
                if !sel.is_empty() {
                    let a = sel.start().max(ls) - ls;
                    let includes_eol = sel.end() > ls + lchars && l + 1 < total;
                    let b = (sel.end() - ls).min(lchars);
                    if b > a || includes_eol {
                        let ua = utf16_of_char_col(text, a);
                        let ub = if includes_eol {
                            utf16_len(text) + 1
                        } else {
                            utf16_of_char_col(text, b)
                        };
                        self.row_sels[r].push((ua, ub));
                    }
                }
                if l == head_line {
                    let col = sel.head - ls;
                    let u = utf16_of_char_col(text, col);
                    if self.caret_on {
                        self.row_carets[r].push(u);
                    }
                    if i == primary {
                        self.ime_anchor = Some((r, u));
                    }
                }
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

    /// Les rangées préparées par [`build_view`](Self::build_view).
    #[must_use]
    pub fn view(&self) -> ViewRows<'_> {
        ViewRows { editor: self }
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
        Row {
            text: &e.row_text[index],
            selections: &e.row_sels[index],
            carets: &e.row_carets[index],
            underline: e.row_underline[index],
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
        assert_eq!(e.title(), "Sans titre* — PaulWrite");
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
    fn titre_avec_nom_de_fichier() {
        let e = Editor::from_document(Document::new(), Some(PathBuf::from("dossier/notes.txt")));
        assert_eq!(e.title(), "notes.txt — PaulWrite");
    }
}
