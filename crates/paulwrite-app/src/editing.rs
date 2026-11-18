//! Opérations d'édition « de tous les jours » : lignes (dupliquer, supprimer, déplacer),
//! commentaires, indentation, saut de ligne avec indentation, paires de parenthèses et de
//! guillemets, sélection de la ligne ou de l'occurrence suivante.
//!
//! Chaque fonction lit le [`Document`], calcule des remplacements simultanés et les applique **en
//! une seule transaction** (un seul pas d'annulation, tous les curseurs traités ensemble) avec
//! [`Document::apply_replacements`]. Aucune dépendance à winit : entièrement testable.

use paulwrite_core::{Buffer, Document, EditKind, Replacement, Selection, SelectionSet};

/// Une indentation.
pub const INDENT: &str = "    ";
/// Largeur d'une tabulation, en colonnes.
pub const TAB_WIDTH: usize = 4;
/// Nombre de caractères parcourus au plus pour chercher l'occurrence suivante (Ctrl+D).
const FIND_WINDOW: usize = 2_000_000;

/// Façon de commenter un langage : un préfixe de ligne (`//`, `#`) ou, à défaut, un couple
/// ouvrant/fermant que l'on pose sur chaque ligne (`<!-- … -->`, `/* … */`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommentStyle {
    pub line: Option<&'static str>,
    pub block: Option<(&'static str, &'static str)>,
}

impl CommentStyle {
    pub const SLASHES: Self = Self {
        line: Some("//"),
        block: None,
    };
    pub const HASH: Self = Self {
        line: Some("#"),
        block: None,
    };
    pub const DASHES: Self = Self {
        line: Some("--"),
        block: None,
    };
    pub const HTML: Self = Self {
        line: None,
        block: Some(("<!--", "-->")),
    };
    pub const CSS: Self = Self {
        line: None,
        block: Some(("/*", "*/")),
    };
}

/// Façon de commenter chaque langage ; sans langage reconnu, `#` (fichiers de configuration).
#[must_use]
pub fn comment_style(language: Option<paulwrite_syntax::Language>) -> CommentStyle {
    use paulwrite_syntax::Language as L;
    match language {
        Some(L::Rust | L::JavaScript | L::Php) => CommentStyle::SLASHES,
        Some(L::Python | L::Toml | L::Yaml | L::Shell) | None => CommentStyle::HASH,
        Some(L::Html | L::Xml | L::Markdown) => CommentStyle::HTML,
        Some(L::Sql) => CommentStyle::DASHES,
        Some(L::Css) => CommentStyle::CSS,
        Some(L::Json) => CommentStyle::default(),
    }
}

// --- Aides ---------------------------------------------------------------------------------------

fn line_string(buf: &Buffer, line: usize) -> String {
    let mut s = String::new();
    for chunk in buf.line(line).chunks() {
        s.push_str(chunk);
    }
    while s.ends_with(['\n', '\r']) {
        s.pop();
    }
    s
}

/// Fin (exclue) du bloc de lignes qui finit à `last` : début de la ligne suivante, ou fin du
/// document.
fn block_end(buf: &Buffer, last: usize) -> usize {
    if last + 1 < buf.len_lines() {
        buf.line_start(last + 1)
    } else {
        buf.len_chars()
    }
}

/// Lignes touchées par les sélections, fusionnées : `(première, dernière)` incluses. Une sélection
/// qui finit au tout début d'une ligne n'inclut pas cette ligne (comme Sublime Text).
fn blocks(doc: &Document) -> Vec<(usize, usize)> {
    let buf = doc.buffer();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for s in doc.selections() {
        let a = buf.char_to_line(s.start());
        let mut b = buf.char_to_line(s.end());
        if !s.is_empty() && b > a && s.end() == buf.line_start(b) {
            b -= 1;
        }
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Sélections d'avant, avec chaque extrémité passée dans `map`.
fn mapped(doc: &Document, map: impl Fn(usize) -> usize) -> SelectionSet {
    let sels = doc
        .selections()
        .iter()
        .map(|s| Selection::new(map(s.anchor), map(s.head)))
        .collect();
    SelectionSet::from_vec(sels, doc.selections().primary_index())
}

fn indent_chars(text: &str) -> usize {
    text.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

// --- Lignes --------------------------------------------------------------------------------------

/// Dupliquer la ou les lignes touchées ; le curseur passe dans la copie.
pub fn duplicate_lines(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let eol = doc.line_ending().as_str();
    let total = buf.len_lines();
    let mut repl: Vec<Replacement> = Vec::new();
    let mut spans: Vec<(usize, usize, usize)> = Vec::new(); // (début, fin, longueur insérée)
    for (a, b) in blocks(doc) {
        let (start, end) = (buf.line_start(a), block_end(buf, b));
        let text = buf.slice(start..end).to_string();
        // dernière ligne sans saut de ligne : il faut en mettre un avant la copie
        let inserted = if b + 1 >= total && !text.ends_with('\n') {
            format!("{eol}{text}")
        } else {
            text
        };
        spans.push((start, end, inserted.chars().count()));
        repl.push((end, end, inserted));
    }
    let map = |pos: usize| {
        let base = Document::map_position(&repl, pos);
        // dans un bloc : le curseur va dans la copie
        match spans.iter().find(|(s, e, _)| pos >= *s && pos < *e) {
            Some((_, _, n)) => base + n,
            None => base,
        }
    };
    let sels = mapped(doc, map);
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

/// Supprimer la ou les lignes touchées ; le curseur va sur la ligne suivante (ou précédente en
/// fin de document), même colonne.
pub fn delete_lines(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let total = buf.len_lines();
    let primary_col = {
        let h = doc.selections().primary().head;
        h - buf.line_start(buf.char_to_line(h))
    };
    let mut repl: Vec<Replacement> = Vec::new();
    let mut carets: Vec<(usize, usize)> = Vec::new(); // (position d'avant, colonne)
    let mut shift: isize = 0;
    for (a, b) in blocks(doc) {
        let (start, end) = (buf.line_start(a), block_end(buf, b));
        if start == end {
            continue;
        }
        // où va le curseur : ligne qui suit le bloc, sinon celle qui le précède
        let (target, len) = if b + 1 < total {
            (start, buf.line_len(b + 1))
        } else if a > 0 {
            (buf.line_start(a - 1), buf.line_len(a - 1))
        } else {
            (start, 0)
        };
        let col = primary_col.min(len);
        let new_target = (target as isize + shift) as usize;
        // la ligne précédente est avant le bloc : sa position n'est pas décalée par ce bloc
        carets.push((new_target + col, 0));
        shift -= (end - start) as isize;
        repl.push((start, end, String::new()));
    }
    if repl.is_empty() {
        return false;
    }
    let sels = SelectionSet::from_vec(
        carets.iter().map(|(p, _)| Selection::caret(*p)).collect(),
        carets.len() - 1,
    );
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

/// Déplacer la ou les lignes touchées d'une ligne vers le haut ou le bas.
pub fn move_lines(doc: &mut Document, up: bool, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let eol = doc.line_ending().as_str();
    let eol_len = eol.chars().count();
    let total = buf.len_lines();
    let mut repl: Vec<Replacement> = Vec::new();
    // (début, fin, décalage appliqué aux positions du bloc)
    let mut moved: Vec<(usize, usize, isize)> = Vec::new();
    for (a, b) in blocks(doc) {
        let neighbour = if up {
            match a.checked_sub(1) {
                Some(n) => n,
                None => continue,
            }
        } else if b + 1 < total {
            b + 1
        } else {
            continue;
        };
        let block: Vec<String> = (a..=b).map(|l| line_string(buf, l)).collect();
        let other = line_string(buf, neighbour);
        let (lo, hi) = if up { (neighbour, b) } else { (a, neighbour) };
        let mut lines: Vec<String> = Vec::with_capacity(block.len() + 1);
        if up {
            lines.extend(block);
            lines.push(other.clone());
        } else {
            lines.push(other.clone());
            lines.extend(block);
        }
        let mut text = lines.join(eol);
        if hi + 1 < total {
            text.push_str(eol);
        }
        let (start, end) = (buf.line_start(lo), block_end(buf, hi));
        let delta = (other.chars().count() + eol_len) as isize;
        repl.push((start, end, text));
        let (bs, be) = (buf.line_start(a), block_end(buf, b));
        moved.push((bs, be, if up { -delta } else { delta }));
    }
    if repl.is_empty() {
        return false;
    }
    // le texte remplacé a la même longueur : seules les positions du bloc bougent
    let sels = mapped(doc, |pos| {
        match moved.iter().find(|(s, e, _)| pos >= *s && pos < *e) {
            Some((_, _, d)) => (pos as isize + d) as usize,
            None => pos,
        }
    });
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

/// Sélectionner les lignes entières ; une deuxième fois, étendre d'une ligne.
pub fn select_lines(doc: &mut Document) {
    let buf = doc.buffer();
    let sels: Vec<Selection> = doc
        .selections()
        .iter()
        .map(|s| {
            let a = buf.char_to_line(s.start());
            let mut b = buf.char_to_line(s.end());
            if !s.is_empty() && b > a && s.end() == buf.line_start(b) {
                b -= 1;
            }
            let (start, end) = (buf.line_start(a), block_end(buf, b));
            // déjà exactement sur des lignes entières : on étend d'une ligne
            if (s.start(), s.end()) == (start, end) && b + 1 < buf.len_lines() {
                Selection::new(start, block_end(buf, b + 1))
            } else {
                Selection::new(start, end)
            }
        })
        .collect();
    let primary = doc.selections().primary_index();
    doc.set_selections(SelectionSet::from_vec(sels, primary));
}

// --- Commentaires --------------------------------------------------------------------------------

/// Commenter ou décommenter les lignes touchées : si toutes les lignes non vides sont déjà
/// commentées on les décommente, sinon on commente tout au niveau d'indentation le plus faible.
pub fn toggle_comment(doc: &mut Document, style: CommentStyle, now_ms: u64) -> bool {
    let (open, close) = match (style.line, style.block) {
        (Some(p), _) => (p, None),
        (None, Some((o, c))) => (o, Some(c)),
        (None, None) => return false,
    };
    let buf = doc.buffer();
    // lignes non vides : (début de ligne, texte, indentation en caractères)
    let mut lines: Vec<(usize, String, usize)> = Vec::new();
    for (a, b) in blocks(doc) {
        for l in a..=b {
            let text = line_string(buf, l);
            if !text.trim().is_empty() {
                let indent = indent_chars(&text);
                lines.push((buf.line_start(l), text, indent));
            }
        }
    }
    if lines.is_empty() {
        return false;
    }
    let commented = |text: &str| {
        let t = text.trim();
        t.starts_with(open) && close.map_or(true, |c| t.ends_with(c))
    };
    let all_commented = lines.iter().all(|(_, t, _)| commented(t));
    let mut repl: Vec<Replacement> = Vec::new();
    if all_commented {
        for (ls, text, indent) in &lines {
            let chars: Vec<char> = text.chars().collect();
            let open_len = open.chars().count();
            let space = usize::from(chars.get(indent + open_len) == Some(&' '));
            repl.push((ls + indent, ls + indent + open_len + space, String::new()));
            if let Some(c) = close {
                let close_len = c.chars().count();
                let trimmed =
                    chars.len() - chars.iter().rev().take_while(|c| c.is_whitespace()).count();
                let end = ls + trimmed;
                let before_space =
                    usize::from(trimmed > close_len && chars[trimmed - close_len - 1] == ' ');
                repl.push((end - close_len - before_space, end, String::new()));
            }
        }
    } else {
        // préfixe de ligne : au niveau d'indentation le plus faible ; couple ouvrant/fermant :
        // autour du texte de chaque ligne
        let level = lines.iter().map(|(_, _, i)| *i).min().unwrap_or(0);
        for (ls, text, indent) in &lines {
            let at = if close.is_some() { *indent } else { level };
            repl.push((ls + at, ls + at, format!("{open} ")));
            if let Some(c) = close {
                let end = ls + text.trim_end().chars().count();
                repl.push((end, end, format!(" {c}")));
            }
        }
    }
    let sels = mapped(doc, |pos| Document::map_position(&repl, pos));
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

// --- Indentation ---------------------------------------------------------------------------------

/// Indenter les lignes touchées (les lignes vides restent vides).
pub fn indent_lines(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let mut repl: Vec<Replacement> = Vec::new();
    for (a, b) in blocks(doc) {
        for l in a..=b {
            if buf.line_len(l) > 0 {
                let ls = buf.line_start(l);
                repl.push((ls, ls, INDENT.to_owned()));
            }
        }
    }
    let sels = mapped(doc, |pos| Document::map_position(&repl, pos));
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

/// Désindenter les lignes touchées : une tabulation, ou jusqu'à [`TAB_WIDTH`] espaces.
pub fn unindent_lines(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let mut repl: Vec<Replacement> = Vec::new();
    for (a, b) in blocks(doc) {
        for l in a..=b {
            let text = line_string(buf, l);
            let remove = if text.starts_with('\t') {
                1
            } else {
                text.chars()
                    .take(TAB_WIDTH)
                    .take_while(|c| *c == ' ')
                    .count()
            };
            if remove > 0 {
                let ls = buf.line_start(l);
                repl.push((ls, ls + remove, String::new()));
            }
        }
    }
    let sels = mapped(doc, |pos| Document::map_position(&repl, pos));
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

/// Touche Tab : avec une sélection sur plusieurs lignes, indente ; sinon remplace la sélection par
/// des espaces jusqu'à la prochaine tabulation.
pub fn insert_tab(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let multi = doc
        .selections()
        .iter()
        .any(|s| !s.is_empty() && buf.char_to_line(s.start()) != buf.char_to_line(s.end()));
    if multi {
        return indent_lines(doc, now_ms);
    }
    let mut repl: Vec<Replacement> = Vec::new();
    let mut carets: Vec<usize> = Vec::new();
    let mut shift: isize = 0;
    for s in doc.selections() {
        let (start, end) = (s.start(), s.end());
        let col = start - buf.line_start(buf.char_to_line(start));
        let width = TAB_WIDTH - col % TAB_WIDTH;
        repl.push((start, end, " ".repeat(width)));
        carets.push((start as isize + shift) as usize + width);
        shift += width as isize - (end - start) as isize;
    }
    let sels = SelectionSet::from_vec(
        carets.into_iter().map(Selection::caret).collect(),
        doc.selections().primary_index(),
    );
    doc.apply_replacements(&repl, sels, EditKind::Typing, now_ms)
}

/// Touche Entrée : saut de ligne qui reprend l'indentation de la ligne ; après `{`, `[` ou `(`,
/// une indentation de plus ; entre deux signes qui se referment (`{|}`), la fermeture passe sur sa
/// propre ligne.
pub fn newline_with_indent(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let eol = doc.line_ending().as_str();
    let eol_len = eol.chars().count();
    let len = buf.len_chars();
    let mut repl: Vec<Replacement> = Vec::new();
    let mut carets: Vec<usize> = Vec::new();
    let mut shift: isize = 0;
    for s in doc.selections() {
        let (start, end) = (s.start(), s.end());
        let line = buf.char_to_line(start);
        let ls = buf.line_start(line);
        // indentation de la ligne, limitée à ce qui précède le curseur
        let indent: String = buf
            .slice(ls..start)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let before = (start > ls).then(|| buf.char(start - 1));
        let after = (end < len).then(|| buf.char(end));
        let opener = matches!(before, Some('{' | '[' | '('));
        let closes = matches!(
            (before, after),
            (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
        );
        let extra = if opener { INDENT } else { "" };
        let (text, caret_offset) = if closes {
            let first = format!("{eol}{indent}{extra}");
            let n = first.chars().count();
            (format!("{first}{eol}{indent}"), n)
        } else {
            let t = format!("{eol}{indent}{extra}");
            let n = t.chars().count();
            (t, n)
        };
        let _ = eol_len;
        carets.push((start as isize + shift) as usize + caret_offset);
        shift += text.chars().count() as isize - (end - start) as isize;
        repl.push((start, end, text));
    }
    let sels = SelectionSet::from_vec(
        carets.into_iter().map(Selection::caret).collect(),
        doc.selections().primary_index(),
    );
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

// --- Paires de signes ----------------------------------------------------------------------------

const PAIRS: [(char, char); 6] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('"', '"'),
    ('\'', '\''),
    ('`', '`'),
];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Frappe d'un caractère avec les paires automatiques. Rend `true` si la frappe a été traitée
/// ici (paire insérée, sélection entourée, fermeture enjambée) ; `false` : insertion ordinaire.
pub fn type_char(doc: &mut Document, c: char, now_ms: u64) -> bool {
    let opener = PAIRS.iter().find(|(o, _)| *o == c).copied();
    let is_closer = PAIRS.iter().any(|(_, cl)| *cl == c);
    if opener.is_none() && !is_closer {
        return false;
    }
    let buf = doc.buffer();
    let len = buf.len_chars();
    // Fermeture déjà là : on passe devant au lieu de la doubler.
    if is_closer
        && doc
            .selections()
            .iter()
            .all(|s| s.is_empty() && s.head < len && buf.char(s.head) == c)
    {
        let sels = doc
            .selections()
            .iter()
            .map(|s| Selection::caret(s.head + 1))
            .collect();
        doc.set_selections(SelectionSet::from_vec(
            sels,
            doc.selections().primary_index(),
        ));
        return true;
    }
    let Some((open, close)) = opener else {
        return false;
    };
    let quote = open == close;
    let mut repl: Vec<Replacement> = Vec::new();
    let mut sels: Vec<Selection> = Vec::new();
    let mut shift: isize = 0;
    let mut special = false;
    for s in doc.selections() {
        let (start, end) = (s.start(), s.end());
        let pos = |p: usize| (p as isize + shift) as usize;
        if !s.is_empty() {
            // entoure la sélection, qui reste sélectionnée
            repl.push((start, start, open.to_string()));
            repl.push((end, end, close.to_string()));
            sels.push(Selection::new(pos(start) + 1, pos(end) + 1));
            shift += 2;
            special = true;
            continue;
        }
        let before = (start > 0).then(|| buf.char(start - 1));
        let after = (start < len).then(|| buf.char(start));
        let after_ok = after.map_or(true, |a| {
            a.is_whitespace() || matches!(a, ')' | ']' | '}' | '>' | ',' | ';' | ':' | '.')
        });
        let before_ok = !quote || before.map_or(true, |b| !is_word(b) && b != open);
        if after_ok && before_ok {
            repl.push((start, start, format!("{open}{close}")));
            sels.push(Selection::caret(pos(start) + 1));
            shift += 2;
            special = true;
        } else {
            repl.push((start, start, open.to_string()));
            sels.push(Selection::caret(pos(start) + 1));
            shift += 1;
        }
    }
    let kind = if special {
        EditKind::Other
    } else {
        EditKind::Typing
    };
    let sels = SelectionSet::from_vec(sels, doc.selections().primary_index());
    doc.apply_replacements(&repl, sels, kind, now_ms)
}

/// Retour arrière entre deux signes d'une paire (`(|)`) : supprime les deux.
pub fn backspace_pair(doc: &mut Document, now_ms: u64) -> bool {
    let buf = doc.buffer();
    let len = buf.len_chars();
    let all_pairs = doc.selections().iter().all(|s| {
        s.is_empty()
            && s.head > 0
            && s.head < len
            && PAIRS.contains(&(buf.char(s.head - 1), buf.char(s.head)))
    });
    if !all_pairs {
        return false;
    }
    let repl: Vec<Replacement> = doc
        .selections()
        .iter()
        .map(|s| (s.head - 1, s.head + 1, String::new()))
        .collect();
    let sels = mapped(doc, |pos| Document::map_position(&repl, pos));
    doc.apply_replacements(&repl, sels, EditKind::Other, now_ms)
}

// --- Occurrences ---------------------------------------------------------------------------------

/// Ctrl+D : sans sélection, sélectionne le mot sous le curseur ; avec une sélection, ajoute la
/// prochaine occurrence du texte sélectionné (en bouclant). Recherche bornée à [`FIND_WINDOW`]
/// caractères. Rend `false` s'il n'y a rien à faire.
pub fn select_next_occurrence(doc: &mut Document) -> bool {
    let buf = doc.buffer();
    let len = buf.len_chars();
    let primary = doc.selections().primary();
    if primary.is_empty() {
        let head = primary.head;
        let mut a = head;
        while a > 0 && is_word(buf.char(a - 1)) {
            a -= 1;
        }
        let mut b = head;
        while b < len && is_word(buf.char(b)) {
            b += 1;
        }
        if a == b {
            return false;
        }
        doc.set_selections(SelectionSet::single(Selection::new(a, b)));
        return true;
    }
    let needle = buf.slice(primary.start()..primary.end()).to_string();
    if needle.contains('\n') {
        return false;
    }
    let n_chars = needle.chars().count();
    // d'abord après la sélection principale, puis depuis le début du document
    let find = |from: usize, to: usize| -> Option<usize> {
        let hay = buf.slice(from..to.min(from + FIND_WINDOW)).to_string();
        hay.find(&needle)
            .map(|byte| from + hay[..byte].chars().count())
    };
    let found = find(primary.end(), len).or_else(|| find(0, primary.start()));
    let Some(at) = found else {
        return false;
    };
    // déjà sélectionnée : rien de nouveau
    if doc
        .selections()
        .iter()
        .any(|s| s.start() == at && s.end() == at + n_chars)
    {
        return false;
    }
    let mut sels: Vec<Selection> = doc.selections().iter().copied().collect();
    sels.push(Selection::new(at, at + n_chars));
    let last = sels.len() - 1;
    doc.set_selections(SelectionSet::from_vec(sels, last));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str, sel: &[(usize, usize)]) -> Document {
        let mut d = Document::from_text(text);
        d.set_selections(SelectionSet::from_vec(
            sel.iter().map(|&(a, h)| Selection::new(a, h)).collect(),
            0,
        ));
        d
    }

    fn text(d: &Document) -> String {
        d.buffer().to_string()
    }

    fn heads(d: &Document) -> Vec<(usize, usize)> {
        d.selections().iter().map(|s| (s.anchor, s.head)).collect()
    }

    #[test]
    fn dupliquer_une_ligne_et_plusieurs() {
        let mut d = doc("a\nb\nc", &[(2, 2)]);
        assert!(duplicate_lines(&mut d, 0));
        assert_eq!(text(&d), "a\nb\nb\nc");
        assert_eq!(heads(&d), [(4, 4)], "le curseur est dans la copie");
        assert!(d.undo());
        assert_eq!(text(&d), "a\nb\nc");
        // sélection sur deux lignes
        let mut d = doc("a\nb\nc\n", &[(0, 3)]);
        duplicate_lines(&mut d, 0);
        assert_eq!(text(&d), "a\nb\na\nb\nc\n");
        // dernière ligne sans saut de ligne
        let mut d = doc("a\nb", &[(3, 3)]);
        duplicate_lines(&mut d, 0);
        assert_eq!(text(&d), "a\nb\nb");
        // plusieurs curseurs sur des lignes différentes : une seule transaction
        let mut d = doc("a\nb\nc\nd\n", &[(0, 0), (4, 4)]);
        duplicate_lines(&mut d, 0);
        assert_eq!(text(&d), "a\na\nb\nc\nc\nd\n");
        assert_eq!(d.history().undo_depth(), 1);
    }

    #[test]
    fn supprimer_des_lignes() {
        let mut d = doc("aa\nbb\ncc\n", &[(4, 4)]);
        assert!(delete_lines(&mut d, 0));
        assert_eq!(text(&d), "aa\ncc\n");
        assert_eq!(heads(&d), [(4, 4)], "même colonne sur la ligne suivante");
        // dernière ligne : le curseur remonte
        let mut d = doc("aa\nbb", &[(4, 4)]);
        delete_lines(&mut d, 0);
        assert_eq!(text(&d), "aa\n");
        assert_eq!(heads(&d), [(1, 1)]);
        // une sélection qui finit au début d'une ligne ne la supprime pas
        let mut d = doc("a\nb\nc\n", &[(0, 2)]);
        delete_lines(&mut d, 0);
        assert_eq!(text(&d), "b\nc\n");
        // document d'une seule ligne
        let mut d = doc("seul", &[(2, 2)]);
        delete_lines(&mut d, 0);
        assert_eq!(text(&d), "");
    }

    #[test]
    fn deplacer_des_lignes() {
        let mut d = doc("a\nb\nc\n", &[(2, 2)]);
        assert!(move_lines(&mut d, true, 0));
        assert_eq!(text(&d), "b\na\nc\n");
        assert_eq!(heads(&d), [(0, 0)], "le curseur suit sa ligne");
        assert!(!move_lines(&mut d, true, 1), "déjà en haut");
        assert!(move_lines(&mut d, false, 2));
        assert_eq!(text(&d), "a\nb\nc\n");
        assert_eq!(heads(&d), [(2, 2)]);
        // vers le bas jusqu'à la dernière ligne, sans saut de ligne final
        let mut d = doc("a\nb\nc", &[(2, 2)]);
        move_lines(&mut d, false, 0);
        assert_eq!(text(&d), "a\nc\nb");
        assert_eq!(heads(&d), [(4, 4)]);
        assert!(!move_lines(&mut d, false, 1), "déjà en bas");
        // un bloc de deux lignes
        let mut d = doc("a\nb\nc\nd\n", &[(2, 5)]);
        move_lines(&mut d, true, 0);
        assert_eq!(text(&d), "b\nc\na\nd\n");
        // annuler d'un coup
        assert!(d.undo());
        assert_eq!(text(&d), "a\nb\nc\nd\n");
    }

    #[test]
    fn commenter_et_decommenter() {
        let mut d = doc("fn a() {\n    x();\n}\n", &[(0, 18)]);
        assert!(toggle_comment(&mut d, CommentStyle::SLASHES, 0));
        assert_eq!(text(&d), "// fn a() {\n//     x();\n}\n");
        assert!(toggle_comment(&mut d, CommentStyle::SLASHES, 1));
        assert_eq!(text(&d), "fn a() {\n    x();\n}\n");
        // niveau d'indentation le plus faible, lignes vides ignorées
        let mut d = doc("    a\n\n      b\n", &[(0, 13)]);
        toggle_comment(&mut d, CommentStyle::HASH, 0);
        assert_eq!(text(&d), "    # a\n\n    #   b\n");
        // une seule ligne commentée sur deux : on commente tout
        let mut d = doc("// a\nb\n", &[(0, 6)]);
        toggle_comment(&mut d, CommentStyle::SLASHES, 0);
        assert_eq!(text(&d), "// // a\n// b\n");
    }

    #[test]
    fn commentaires_de_bloc() {
        let mut d = doc("<p>x</p>\n  <b>y</b>\n", &[(0, 18)]);
        assert!(toggle_comment(&mut d, CommentStyle::HTML, 0));
        assert_eq!(text(&d), "<!-- <p>x</p> -->\n  <!-- <b>y</b> -->\n");
        assert!(toggle_comment(&mut d, CommentStyle::HTML, 1));
        assert_eq!(text(&d), "<p>x</p>\n  <b>y</b>\n");
        let mut d = doc("a { b: c }\n", &[(0, 0)]);
        toggle_comment(&mut d, CommentStyle::CSS, 0);
        assert_eq!(text(&d), "/* a { b: c } */\n");
        assert!(
            !toggle_comment(&mut d, CommentStyle::default(), 1),
            "langage sans commentaire"
        );
    }

    #[test]
    fn indenter_et_desindenter() {
        let mut d = doc("a\n\nb\n", &[(0, 4)]);
        assert!(indent_lines(&mut d, 0));
        assert_eq!(text(&d), "    a\n\n    b\n");
        assert!(unindent_lines(&mut d, 1));
        assert_eq!(text(&d), "a\n\nb\n");
        // tabulation et indentation partielle
        let mut d = doc("\ta\n  b\n        c\n", &[(0, 15)]);
        unindent_lines(&mut d, 0);
        assert_eq!(text(&d), "a\nb\n    c\n");
        assert!(unindent_lines(&mut d, 1));
        assert_eq!(text(&d), "a\nb\nc\n");
    }

    #[test]
    fn la_touche_tab() {
        // sans sélection : jusqu'à la prochaine tabulation
        let mut d = doc("ab", &[(2, 2)]);
        insert_tab(&mut d, 0);
        assert_eq!(text(&d), "ab  ");
        insert_tab(&mut d, 1);
        assert_eq!(text(&d), "ab      ");
        // sélection sur plusieurs lignes : indentation
        let mut d = doc("a\nb\n", &[(0, 3)]);
        insert_tab(&mut d, 0);
        assert_eq!(text(&d), "    a\n    b\n");
        // sélection sur une ligne : remplacée
        let mut d = doc("xyz", &[(1, 2)]);
        insert_tab(&mut d, 0);
        assert_eq!(text(&d), "x   z");
    }

    #[test]
    fn saut_de_ligne_avec_indentation() {
        let mut d = doc("    abc", &[(7, 7)]);
        newline_with_indent(&mut d, 0);
        assert_eq!(text(&d), "    abc\n    ");
        assert_eq!(heads(&d), [(12, 12)]);
        // après une accolade : une indentation de plus
        let mut d = doc("fn a() {", &[(8, 8)]);
        newline_with_indent(&mut d, 0);
        assert_eq!(text(&d), "fn a() {\n    ");
        // entre deux accolades : la fermeture passe à la ligne
        let mut d = doc("  {}", &[(3, 3)]);
        newline_with_indent(&mut d, 0);
        assert_eq!(text(&d), "  {\n      \n  }");
        assert_eq!(
            heads(&d),
            [(10, 10)],
            "le curseur est sur la ligne du milieu"
        );
        // le milieu d'une ligne : l'indentation ne dépasse pas le curseur
        let mut d = doc("    abc", &[(2, 2)]);
        newline_with_indent(&mut d, 0);
        assert_eq!(text(&d), "  \n    abc");
        // plusieurs curseurs
        let mut d = doc("  a\n  b", &[(3, 3), (7, 7)]);
        newline_with_indent(&mut d, 0);
        assert_eq!(text(&d), "  a\n  \n  b\n  ");
        assert_eq!(d.history().undo_depth(), 1);
        // fins de ligne CRLF du document
        let mut d = Document::from_buffer(
            paulwrite_core::Buffer::from_text("x\r\n"),
            paulwrite_core::Encoding::UTF8,
            paulwrite_core::LineEnding::CrLf,
        );
        d.set_selections(SelectionSet::single(Selection::caret(1)));
        newline_with_indent(&mut d, 0);
        assert_eq!(text(&d), "x\r\n\r\n");
    }

    #[test]
    fn paires_de_signes() {
        // une paire, curseur au milieu
        let mut d = doc("", &[(0, 0)]);
        assert!(type_char(&mut d, '(', 0));
        assert_eq!(text(&d), "()");
        assert_eq!(heads(&d), [(1, 1)]);
        // taper la fermeture : on l'enjambe
        assert!(type_char(&mut d, ')', 1));
        assert_eq!(text(&d), "()");
        assert_eq!(heads(&d), [(2, 2)]);
        // guillemets : pas dans un mot (apostrophe), oui après un espace
        let mut d = doc("l", &[(1, 1)]);
        assert!(type_char(&mut d, '\'', 0));
        assert_eq!(text(&d), "l'", "apostrophe d'un mot : pas de paire");
        let mut d = doc("a ", &[(2, 2)]);
        assert!(type_char(&mut d, '"', 0));
        assert_eq!(text(&d), "a \"\"");
        // devant du texte : seulement le signe ouvrant
        let mut d = doc("x", &[(0, 0)]);
        type_char(&mut d, '[', 0);
        assert_eq!(text(&d), "[x");
        // la sélection est entourée et reste sélectionnée
        let mut d = doc("abc", &[(0, 3)]);
        assert!(type_char(&mut d, '{', 0));
        assert_eq!(text(&d), "{abc}");
        assert_eq!(heads(&d), [(1, 4)]);
        // autres caractères : traitement ordinaire
        let mut d = doc("", &[(0, 0)]);
        assert!(!type_char(&mut d, 'a', 0));
        assert!(!type_char(&mut d, ')', 0), "fermeture sans rien à enjamber");
        // plusieurs curseurs
        let mut d = doc("\n", &[(0, 0), (1, 1)]);
        type_char(&mut d, '(', 0);
        assert_eq!(text(&d), "()\n()");
        assert_eq!(d.history().undo_depth(), 1);
    }

    #[test]
    fn retour_arriere_dans_une_paire() {
        let mut d = doc("a()b", &[(2, 2)]);
        assert!(backspace_pair(&mut d, 0));
        assert_eq!(text(&d), "ab");
        assert_eq!(heads(&d), [(1, 1)]);
        let mut d = doc("a(x)b", &[(2, 2)]);
        assert!(!backspace_pair(&mut d, 0), "pas une paire vide");
        let mut d = doc("\"\"", &[(1, 1)]);
        assert!(backspace_pair(&mut d, 0));
        assert_eq!(text(&d), "");
        let mut d = doc("()", &[(0, 2)]);
        assert!(
            !backspace_pair(&mut d, 0),
            "avec une sélection : suppression ordinaire"
        );
    }

    #[test]
    fn selection_des_lignes() {
        let mut d = doc("aa\nbb\ncc\n", &[(1, 1)]);
        select_lines(&mut d);
        assert_eq!(heads(&d), [(0, 3)]);
        select_lines(&mut d);
        assert_eq!(heads(&d), [(0, 6)], "une deuxième fois : une ligne de plus");
    }

    #[test]
    fn occurrence_suivante() {
        let mut d = doc("foo bar foo baz foo", &[(1, 1)]);
        // sans sélection : le mot
        assert!(select_next_occurrence(&mut d));
        assert_eq!(heads(&d), [(0, 3)]);
        // puis l'occurrence suivante, ajoutée
        assert!(select_next_occurrence(&mut d));
        assert_eq!(heads(&d), [(0, 3), (8, 11)]);
        assert!(select_next_occurrence(&mut d));
        assert_eq!(heads(&d), [(0, 3), (8, 11), (16, 19)]);
        // toutes sélectionnées : rien de nouveau
        assert!(!select_next_occurrence(&mut d));
        // la recherche boucle au début du document
        let mut d = doc("ab x ab", &[(5, 7)]);
        assert!(select_next_occurrence(&mut d));
        assert_eq!(heads(&d), [(0, 2), (5, 7)]);
        // caractères multi-octets
        let mut d = doc("é-é", &[(0, 1)]);
        assert!(select_next_occurrence(&mut d));
        assert_eq!(heads(&d), [(0, 1), (2, 3)]);
        // curseur au milieu de rien
        let mut d = doc("   ", &[(1, 1)]);
        assert!(!select_next_occurrence(&mut d));
    }
}
