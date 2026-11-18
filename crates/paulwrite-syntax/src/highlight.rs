//! Arbre syntaxique d'un document et calcul des plages colorées d'une fenêtre de texte.

use std::ops::Range;

use paulwrite_core::TextEdit;
use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, Range as TsRange, Tree};

use crate::{kind, Kind, Language};

/// Nombre de catégories de couleur (indices `0..KIND_COUNT`, 0 = texte ordinaire).
pub const KIND_COUNT: usize = 15;

/// Nom d'une catégorie (thèmes, tests).
#[must_use]
pub fn kind_name(k: Kind) -> &'static str {
    match k {
        kind::COMMENT => "comment",
        kind::STRING => "string",
        kind::NUMBER => "number",
        kind::KEYWORD => "keyword",
        kind::FUNCTION => "function",
        kind::TYPE => "type",
        kind::CONSTANT => "constant",
        kind::PROPERTY => "property",
        kind::OPERATOR => "operator",
        kind::PUNCTUATION => "punctuation",
        kind::ATTRIBUTE => "attribute",
        kind::TAG => "tag",
        kind::ESCAPE => "escape",
        kind::BUILTIN => "builtin",
        _ => "text",
    }
}

/// Catégorie d'une capture de requête (`@keyword.control`, `@string.escape`…). On essaie le nom
/// complet puis des préfixes de plus en plus courts.
fn kind_for_capture(name: &str) -> Kind {
    let mut candidate = name;
    loop {
        let k = match candidate {
            "comment" => kind::COMMENT,
            "string" | "string.special" | "string.regexp" | "string.special.symbol" => kind::STRING,
            "string.escape" | "escape" => kind::ESCAPE,
            "number" | "float" | "constant.numeric" => kind::NUMBER,
            "keyword" => kind::KEYWORD,
            "function" => kind::FUNCTION,
            "function.macro" => kind::ATTRIBUTE,
            "function.builtin" | "variable.builtin" => kind::BUILTIN,
            "type" | "constructor" | "module" | "namespace" => kind::TYPE,
            "constant" | "boolean" => kind::CONSTANT,
            "property" | "field" | "variable.member" => kind::PROPERTY,
            "operator" => kind::OPERATOR,
            "punctuation" => kind::PUNCTUATION,
            "attribute" | "label" | "text.uri" => kind::ATTRIBUTE,
            "tag" => kind::TAG,
            // Markdown : titres, code, liens, emphase (les couleurs sont les seules distinctions)
            "text.title" => kind::FUNCTION,
            "text.literal" => kind::STRING,
            "text.reference" => kind::PROPERTY,
            "text.emphasis" => kind::TYPE,
            "text.strong" => kind::CONSTANT,
            _ => 0,
        };
        if k != 0 {
            return k;
        }
        match candidate.rfind('.') {
            Some(i) => candidate = &candidate[..i],
            None => return 0,
        }
    }
}

/// Plage d'octets colorée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

/// Analyseur d'un document : arbre incrémental + requête de coloration.
pub struct Highlighter {
    language: Language,
    parser: Parser,
    tree: Option<Tree>,
    query: Query,
    capture_kinds: Vec<Kind>,
    cursor: QueryCursor,
    /// Langages imbriqués : le HTML des fichiers PHP, le JavaScript des `<script>` et le CSS des
    /// `<style>` (voir [`Embedded`]).
    embedded: Option<Embedded>,
    /// Plages dont la structure a changé au dernier `update` (`None` : inconnues).
    changed: Option<Vec<TsRange>>,
    // tampons réutilisés
    captures: Vec<(usize, usize, usize, Kind)>,
    paint: Vec<Kind>,
}

/// Les langages imbriqués d'un fichier HTML ou PHP. Chaque couche est une seconde analyse
/// tree-sitter **limitée à des plages** de la couche du dessus : le HTML d'un fichier PHP (nœuds
/// `text`), puis le JavaScript des `<script>` et le CSS des `<style>` (nœuds `raw_text`). Une
/// couche n'est créée que s'il y a quelque chose à colorer (la compilation de sa requête n'est pas
/// gratuite), et chaque couche réutilise son ancien arbre : tree-sitter compare les plages
/// incluses et ne reparse que ce qui a changé.
struct Embedded {
    /// Le HTML qui entoure le code PHP (absent pour un fichier HTML : c'est l'arbre principal).
    html: Option<Layer>,
    /// Le texte courant d'un document Markdown (emphase, liens, code en ligne) : une seconde
    /// grammaire, appliquée aux nœuds `inline` de l'arbre des blocs.
    inline: Option<Layer>,
    js: Option<Layer>,
    css: Option<Layer>,
    /// Plages à analyser, réutilisées d'une modification à l'autre.
    ranges: Vec<TsRange>,
    css_ranges: Vec<TsRange>,
    /// Repère les `<script>` et `<style>` du HTML (créé au premier besoin).
    scan: Option<Scan>,
}

/// Une couche de coloration : analyseur, arbre et requête d'un langage imbriqué.
struct Layer {
    parser: Parser,
    tree: Option<Tree>,
    query: Query,
    capture_kinds: Vec<Kind>,
    cursor: QueryCursor,
    /// Départage les motifs de deux couches qui se recouvrent (la couche du dessous gagne).
    base: usize,
    /// Plages dont la structure a changé au dernier `reparse` (`None` : inconnues, analyse complète).
    changed: Option<Vec<TsRange>>,
}

/// Nœuds dont les plages sont analysées par la couche HTML d'un fichier PHP : `(type du nœud, type
/// de son parent)`.
const HTML_IN_PHP: (&str, Option<&str>) = ("text", None);
const MD_INLINE: (&str, Option<&str>) = ("inline", None);

impl Embedded {
    fn new() -> Self {
        Self {
            html: None,
            inline: None,
            js: None,
            css: None,
            ranges: Vec::new(),
            css_ranges: Vec::new(),
            scan: None,
        }
    }

    /// Réanalyse toutes les couches d'après le nouvel arbre principal.
    fn reparse(
        &mut self,
        language: Language,
        main: &Tree,
        main_changed: Option<&[TsRange]>,
        rope: &Rope,
        edits: &[TextEdit],
    ) {
        let Self {
            html,
            inline,
            js,
            css,
            ranges,
            css_ranges,
            scan,
        } = self;
        if language == Language::Markdown {
            ranges.clear();
            collect_ranges(main.root_node(), MD_INLINE, ranges);
            Layer::update(
                inline,
                tree_sitter_md::INLINE_LANGUAGE.into(),
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                1_000_000,
                ranges,
                rope,
                edits,
            );
            return;
        }
        // La couche HTML : l'arbre principal si le fichier est du HTML, sinon celle du PHP.
        let html_tree: Option<&Tree> = if language == Language::Php {
            ranges.clear();
            collect_ranges(main.root_node(), HTML_IN_PHP, ranges);
            Layer::update(
                html,
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY,
                1_000_000,
                ranges,
                rope,
                edits,
            );
            html.as_ref().and_then(|l| l.tree.as_ref())
        } else {
            Some(main)
        };
        // Les `<script>` et les `<style>` : une seule requête pour le JavaScript et le CSS.
        ranges.clear();
        css_ranges.clear();
        if scan.is_none() {
            *scan = Scan::new();
        }
        if let Some(scan) = scan.as_mut() {
            match html_tree {
                Some(tree) => {
                    // seules les zones modifiées sont relues (voir `Scan`)
                    let changed = if language == Language::Php {
                        html.as_ref().and_then(|l| l.changed.as_deref())
                    } else {
                        main_changed
                    };
                    scan.update(tree.root_node(), edits, changed);
                    scan.ranges(rope, ranges, css_ranges);
                }
                None => scan.reset(),
            }
        }
        Layer::update(
            js,
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            2_000_000,
            ranges,
            rope,
            edits,
        );
        Layer::update(
            css,
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
            3_000_000,
            css_ranges,
            rope,
            edits,
        );
    }

    /// Les couches qui ont un arbre, du dessous au dessus.
    fn layers(&mut self) -> impl Iterator<Item = &mut Layer> {
        [
            self.html.as_mut(),
            self.inline.as_mut(),
            self.js.as_mut(),
            self.css.as_mut(),
        ]
        .into_iter()
        .flatten()
        .filter(|l| l.tree.is_some())
    }

    fn reset(&mut self) {
        for l in self.layers() {
            l.tree = None;
        }
        if let Some(scan) = self.scan.as_mut() {
            scan.reset();
        }
    }
}

impl Layer {
    fn new(grammar: &tree_sitter::Language, query: &str, base: usize) -> Result<Self, String> {
        let mut parser = Parser::new();
        parser.set_language(grammar).map_err(|e| e.to_string())?;
        let query = Query::new(grammar, query).map_err(|e| e.to_string())?;
        let capture_kinds = query
            .capture_names()
            .iter()
            .map(|n| kind_for_capture(n))
            .collect();
        Ok(Self {
            parser,
            tree: None,
            query,
            capture_kinds,
            cursor: QueryCursor::new(),
            base,
            changed: None,
        })
    }

    /// Met à jour la couche `slot` pour les plages `ranges` ; la crée au premier besoin, et
    /// l'oublie quand il n'y a plus rien à analyser.
    fn update(
        slot: &mut Option<Layer>,
        grammar: tree_sitter::Language,
        query: &str,
        base: usize,
        ranges: &[TsRange],
        rope: &Rope,
        edits: &[TextEdit],
    ) {
        if ranges.is_empty() {
            if let Some(layer) = slot.as_mut() {
                layer.tree = None;
                layer.changed = None;
            }
            return;
        }
        if slot.is_none() {
            *slot = Layer::new(&grammar, query, base).ok();
        }
        if let Some(layer) = slot.as_mut() {
            layer.reparse(ranges, rope, edits);
        }
    }

    /// Réanalyse les plages `ranges`. L'ancien arbre est réutilisé (modifications appliquées).
    fn reparse(&mut self, ranges: &[TsRange], rope: &Rope, edits: &[TextEdit]) {
        self.changed = None;
        if self.parser.set_included_ranges(ranges).is_err() {
            self.tree = None;
            return;
        }
        if let Some(old) = self.tree.as_mut() {
            for e in edits {
                old.edit(&InputEdit {
                    start_byte: e.start_byte,
                    old_end_byte: e.old_end_byte,
                    new_end_byte: e.new_end_byte,
                    start_position: to_point(e.start_point),
                    old_end_position: to_point(e.old_end_point),
                    new_end_position: to_point(e.new_end_point),
                });
            }
        }
        let len = rope.len_bytes();
        let mut provider = |byte: usize, _p: Point| -> &[u8] {
            if byte >= len {
                return &[];
            }
            let (chunk, start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - start..]
        };
        let old = self.tree.clone();
        self.tree = self
            .parser
            .parse_with_options(&mut provider, self.tree.as_ref(), None);
        self.changed = match (&old, &self.tree) {
            (Some(o), Some(n)) => Some(o.changed_ranges(n).collect()),
            _ => None,
        };
    }
}

/// Plages des nœuds `(type, type du parent)` sous `root`, dans l'ordre du document : le HTML hors
/// des balises `<?php … ?>` (`text`), ou le contenu des `<script>` et `<style>` (`raw_text`). On ne
/// descend pas dans un nœud retenu.
fn collect_ranges(root: Node<'_>, (kind, parent): (&str, Option<&str>), out: &mut Vec<TsRange>) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let wanted = node.kind() == kind
            && parent.map_or(true, |p| node.parent().is_some_and(|n| n.kind() == p));
        if wanted {
            out.push(node.range());
        } else if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Plage d'octets du contenu d'un `<script>` ou d'un `<style>` ; `dirty` : une modification l'a
/// touchée, elle doit être relue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Block {
    start: usize,
    end: usize,
    dirty: bool,
}

/// Repère le contenu des `<script>` et des `<style>` d'un arbre HTML, et le tient à jour d'une
/// modification à l'autre. Une requête tree-sitter (exécutée côté C) coûte bien moins qu'un parcours
/// nœud par nœud, mais relire tout l'arbre à chaque frappe ajoutait encore 30 à 40 % à la
/// coloration d'un gros fichier (mesuré) : on décale donc les plages connues d'après les
/// modifications et on ne relit que les zones où la structure a changé (`changed_ranges`) et les
/// plages touchées.
struct Scan {
    query: Query,
    cursor: QueryCursor,
    js: Vec<Block>,
    css: Vec<Block>,
    /// Les listes décrivent le document précédent (sinon : analyse complète).
    primed: bool,
}

/// Décale les plages d'après une modification ; celles qu'elle touche sont marquées à relire
/// (elles couvrent alors toute la zone modifiée).
fn shift_blocks(blocks: &mut [Block], e: &TextEdit) {
    let delta = e.new_end_byte as isize - e.old_end_byte as isize;
    let moved = |x: usize| (x as isize + delta) as usize;
    for b in blocks {
        if b.end < e.start_byte {
            // avant la modification : inchangée
        } else if b.start > e.old_end_byte {
            b.start = moved(b.start);
            b.end = moved(b.end);
        } else {
            b.start = b.start.min(e.start_byte);
            b.end = moved(b.end.max(e.old_end_byte));
            b.dirty = true;
        }
    }
}

impl Scan {
    fn new() -> Option<Self> {
        let grammar: tree_sitter::Language = tree_sitter_html::LANGUAGE.into();
        let query = Query::new(
            &grammar,
            "(script_element (raw_text) @js) (style_element (raw_text) @css)",
        )
        .ok()?;
        Some(Self {
            query,
            cursor: QueryCursor::new(),
            js: Vec::new(),
            css: Vec::new(),
            primed: false,
        })
    }

    fn reset(&mut self) {
        self.js.clear();
        self.css.clear();
        self.primed = false;
    }

    /// Relit les plages dans `window` (octets) et les ajoute aux listes.
    fn scan_window(&mut self, root: Node<'_>, window: Range<usize>) {
        self.cursor.set_byte_range(window);
        // la requête n'a pas de prédicat : aucun texte n'est demandé
        let mut matches = self
            .cursor
            .matches(&self.query, root, |_: Node<'_>| std::iter::empty::<&[u8]>());
        while let Some(m) = matches.next() {
            for c in m.captures() {
                let block = Block {
                    start: c.node.start_byte(),
                    end: c.node.end_byte(),
                    dirty: false,
                };
                if c.index == 0 {
                    self.js.push(block);
                } else {
                    self.css.push(block);
                }
            }
        }
    }

    /// Met les listes à jour pour le nouvel arbre `root`. `edits` : les modifications depuis le
    /// dernier appel ; `changed` : plages dont la structure a changé (`None` : on ne sait pas).
    fn update(&mut self, root: Node<'_>, edits: &[TextEdit], changed: Option<&[TsRange]>) {
        let (Some(changed), true) = (changed, self.primed && !edits.is_empty()) else {
            self.js.clear();
            self.css.clear();
            self.scan_window(root, 0..usize::MAX);
            self.primed = true;
            return;
        };
        for e in edits {
            shift_blocks(&mut self.js, e);
            shift_blocks(&mut self.css, e);
        }
        // zones à relire : structure modifiée, et plages touchées (le texte d'un `raw_text` peut
        // changer sans que sa structure change)
        let mut windows: Vec<Range<usize>> = changed
            .iter()
            .map(|r| r.start_byte.saturating_sub(1)..r.end_byte + 1)
            .collect();
        windows.extend(
            self.js
                .iter()
                .chain(&self.css)
                .filter(|b| b.dirty)
                .map(|b| b.start.saturating_sub(1)..b.end + 1),
        );
        let touches = |b: &Block| windows.iter().any(|w| b.end >= w.start && b.start <= w.end);
        self.js.retain(|b| !touches(b));
        self.css.retain(|b| !touches(b));
        for w in windows {
            self.scan_window(root, w);
        }
        for list in [&mut self.js, &mut self.css] {
            list.sort_unstable_by_key(|b| (b.start, b.end));
            list.dedup();
            // jamais deux plages qui se recouvrent (l'analyseur les refuserait)
            let mut last_end = 0;
            list.retain(|b| {
                let keep = b.start >= last_end;
                if keep {
                    last_end = b.end;
                }
                keep
            });
        }
    }

    /// Les plages à donner aux couches JavaScript et CSS (avec leurs positions ligne/colonne).
    fn ranges(&self, rope: &Rope, js: &mut Vec<TsRange>, css: &mut Vec<TsRange>) {
        let len = rope.len_bytes();
        let point = |byte: usize| {
            let byte = byte.min(len);
            let row = rope.byte_to_line(byte);
            Point {
                row,
                column: byte - rope.line_to_byte(row),
            }
        };
        for (blocks, out) in [(&self.js, js), (&self.css, css)] {
            out.clear();
            out.extend(blocks.iter().map(|b| TsRange {
                start_byte: b.start,
                end_byte: b.end.min(len),
                start_point: point(b.start),
                end_point: point(b.end),
            }));
        }
    }
}

/// Ajoute à `out` les captures de `query` sur `tree` dans `range` : `(motif + base, début, fin,
/// catégorie)`. `base` départage les motifs de deux requêtes qui se recouvrent.
#[allow(clippy::too_many_arguments)]
fn collect_captures(
    query: &Query,
    cursor: &mut QueryCursor,
    capture_kinds: &[Kind],
    tree: &Tree,
    rope: &Rope,
    range: &Range<usize>,
    base: usize,
    out: &mut Vec<(usize, usize, usize, Kind)>,
) {
    let len = rope.len_bytes();
    cursor.set_byte_range(range.clone());
    let text = |node: Node<'_>| {
        rope.byte_slice(node.start_byte().min(len)..node.end_byte().min(len))
            .chunks()
            .map(str::as_bytes)
    };
    let mut matches = cursor.matches(query, tree.root_node(), text);
    while let Some(m) = matches.next() {
        for c in m.captures() {
            let mut k = capture_kinds[c.index as usize];
            if k == 0 {
                continue;
            }
            // Les littéraux numériques sont souvent capturés comme « constante » (Rust :
            // `@constant.builtin`) : on les distingue d'après le type du nœud.
            if k == kind::CONSTANT {
                let n = c.node.kind();
                if n.contains("integer") || n.contains("float") || n.contains("number") {
                    k = kind::NUMBER;
                }
            }
            let r = c.node.byte_range();
            out.push((
                base + m.pattern_index,
                r.start.max(range.start),
                r.end.min(range.end),
                k,
            ));
        }
    }
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

fn to_point((row, column): (usize, usize)) -> Point {
    Point { row, column }
}

impl Highlighter {
    /// Crée l'analyseur d'un langage (compile la requête de coloration : à faire hors du thread UI).
    ///
    /// # Errors
    /// Grammaire ou requête incompatible.
    pub fn new(language: Language) -> Result<Self, String> {
        let grammar = language.grammar();
        let mut parser = Parser::new();
        parser.set_language(&grammar).map_err(|e| e.to_string())?;
        let query =
            Query::new(&grammar, &language.highlights_query()).map_err(|e| e.to_string())?;
        let capture_kinds = query
            .capture_names()
            .iter()
            .map(|n| kind_for_capture(n))
            .collect();
        Ok(Self {
            language,
            parser,
            tree: None,
            query,
            capture_kinds,
            cursor: QueryCursor::new(),
            embedded: matches!(
                language,
                Language::Php | Language::Html | Language::Markdown
            )
            .then(Embedded::new),
            changed: None,
            captures: Vec::new(),
            paint: Vec::new(),
        })
    }

    #[must_use]
    pub fn language(&self) -> Language {
        self.language
    }

    /// Met l'arbre à jour : applique les `edits` (dans l'ordre) à l'ancien arbre puis reparse
    /// incrémentalement `rope`. Sans arbre précédent, c'est un parse complet.
    pub fn update(&mut self, rope: &Rope, edits: &[TextEdit]) {
        if let Some(tree) = self.tree.as_mut() {
            for e in edits {
                tree.edit(&InputEdit {
                    start_byte: e.start_byte,
                    old_end_byte: e.old_end_byte,
                    new_end_byte: e.new_end_byte,
                    start_position: to_point(e.start_point),
                    old_end_position: to_point(e.old_end_point),
                    new_end_position: to_point(e.new_end_point),
                });
            }
        }
        let len = rope.len_bytes();
        let mut provider = |byte: usize, _p: Point| -> &[u8] {
            if byte >= len {
                return &[];
            }
            let (chunk, start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - start..]
        };
        let old = self.tree.clone();
        self.tree = self
            .parser
            .parse_with_options(&mut provider, self.tree.as_ref(), None);
        self.changed = match (&old, &self.tree) {
            (Some(o), Some(n)) => Some(o.changed_ranges(n).collect()),
            _ => None,
        };
        if let (Some(embedded), Some(tree)) = (self.embedded.as_mut(), self.tree.as_ref()) {
            embedded.reparse(self.language, tree, self.changed.as_deref(), rope, edits);
        }
    }

    /// Oublie l'arbre (document remplacé) : le prochain `update` sera un parse complet.
    pub fn reset(&mut self) {
        self.tree = None;
        self.parser.reset();
        if let Some(embedded) = self.embedded.as_mut() {
            embedded.reset();
        }
    }

    /// Plages colorées intersectant `range` (octets), triées, sans chevauchement, fusionnées.
    pub fn highlights(&mut self, rope: &Rope, range: Range<usize>) -> Vec<Span> {
        let len = rope.len_bytes();
        let range = range.start.min(len)..range.end.min(len);
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        if range.is_empty() {
            return Vec::new();
        }

        let Self {
            query,
            cursor,
            capture_kinds,
            embedded,
            captures,
            paint,
            ..
        } = self;
        captures.clear();
        collect_captures(
            query,
            cursor,
            capture_kinds,
            tree,
            rope,
            &range,
            0,
            captures,
        );
        // les langages imbriqués (HTML d'un fichier PHP, JavaScript, CSS) : leurs motifs passent
        // après ceux de la couche principale (base élevée)
        if let Some(embedded) = embedded {
            for layer in embedded.layers() {
                if let Some(layer_tree) = layer.tree.as_ref() {
                    collect_captures(
                        &layer.query,
                        &mut layer.cursor,
                        &layer.capture_kinds,
                        layer_tree,
                        rope,
                        &range,
                        layer.base,
                        captures,
                    );
                }
            }
        }

        // Aplatit les captures qui se recouvrent en une couleur par octet : le texte le plus
        // **interne** (plus court) l'emporte, et à plage égale le **premier** motif de la requête
        // (convention de `tree-sitter-highlight`, pour lequel les requêtes sont écrites). On peint
        // donc du plus long au plus court, et à longueur égale du dernier au premier motif.
        paint.clear();
        paint.resize(range.len(), 0);
        captures.sort_by(|a, b| (b.2 - b.1).cmp(&(a.2 - a.1)).then_with(|| b.0.cmp(&a.0)));
        for &(_, s, e, k) in captures.iter() {
            if s < e {
                paint[s - range.start..e - range.start].fill(k);
            }
        }

        let mut spans = Vec::new();
        let mut i = 0;
        while i < paint.len() {
            let k = paint[i];
            let start = i;
            while i < paint.len() && paint[i] == k {
                i += 1;
            }
            if k != 0 {
                spans.push(Span {
                    start: range.start + start,
                    end: range.start + i,
                    kind: k,
                });
            }
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paulwrite_core::Buffer;

    fn spans_of(lang: Language, text: &str) -> Vec<(String, &'static str)> {
        let rope = Rope::from_str(text);
        let mut h = Highlighter::new(lang).unwrap();
        h.update(&rope, &[]);
        h.highlights(&rope, 0..text.len())
            .iter()
            .map(|s| (text[s.start..s.end].to_owned(), kind_name(s.kind)))
            .collect()
    }

    #[test]
    fn noms_de_capture() {
        assert_eq!(kind_for_capture("keyword.control.return"), kind::KEYWORD);
        assert_eq!(kind_for_capture("string.escape"), kind::ESCAPE);
        assert_eq!(kind_for_capture("function.macro"), kind::ATTRIBUTE);
        assert_eq!(kind_for_capture("punctuation.bracket"), kind::PUNCTUATION);
        assert_eq!(kind_for_capture("variable.parameter"), 0);
        assert_eq!(kind_for_capture("inconnu"), 0);
    }

    #[test]
    fn rust_colore_mots_cles_nombres_et_commentaires() {
        let s = spans_of(Language::Rust, "fn main() { let x = 42; // salut\n}\n");
        let has = |t: &str, k: &str| s.iter().any(|(a, b)| a == t && *b == k);
        assert!(has("fn", "keyword"), "{s:?}");
        assert!(has("let", "keyword"), "{s:?}");
        assert!(has("42", "number"), "{s:?}");
        assert!(has("// salut", "comment"), "{s:?}");
        assert!(has("main", "function"), "{s:?}");
    }

    #[test]
    fn autres_langages() {
        let json = spans_of(Language::Json, "{\"a\": [1, true, null]}");
        assert!(
            json.iter().any(|(t, k)| t == "1" && *k == "number"),
            "{json:?}"
        );
        let py = spans_of(Language::Python, "def f(x):\n    return 'a' # c\n");
        assert!(
            py.iter().any(|(t, k)| t == "def" && *k == "keyword"),
            "{py:?}"
        );
        assert!(
            py.iter().any(|(t, k)| t == "# c" && *k == "comment"),
            "{py:?}"
        );
        let css = spans_of(Language::Css, "a { color: red; }");
        assert!(!css.is_empty());
        let toml = spans_of(Language::Toml, "[pkg]\nname = \"x\"\nn = 3\n");
        assert!(
            toml.iter().any(|(t, k)| t == "3" && *k == "number"),
            "{toml:?}"
        );
        let js = spans_of(Language::JavaScript, "const a = 1; // x\nfunction f() {}");
        assert!(
            js.iter().any(|(t, k)| t == "const" && *k == "keyword"),
            "{js:?}"
        );
        let html = spans_of(Language::Html, "<div class=\"a\">x</div>");
        assert!(html.iter().any(|(_, k)| *k == "tag"), "{html:?}");
    }

    #[test]
    fn fenetre_partielle() {
        let text = "fn a() {}\nfn b() {}\n// fin\n";
        let rope = Rope::from_str(text);
        let mut h = Highlighter::new(Language::Rust).unwrap();
        h.update(&rope, &[]);
        let all = h.highlights(&rope, 0..text.len());
        let part = h.highlights(&rope, 10..19);
        assert!(!part.is_empty());
        assert!(part.iter().all(|s| s.start >= 10 && s.end <= 19));
        for s in &part {
            assert!(all
                .iter()
                .any(|a| a.kind == s.kind && a.start <= s.start && a.end >= s.end));
        }
        // bornes hors du texte : pas de panique
        let _ = h.highlights(&rope, 0..10_000);
        #[allow(clippy::reversed_empty_ranges)]
        let reversed = 50..40;
        assert!(h.highlights(&rope, reversed).is_empty());
    }

    /// Après des modifications aléatoires, l'analyse incrémentale donne les mêmes couleurs
    /// qu'une analyse complète du même texte.
    #[test]
    fn incremental_egale_complet() {
        let mut buf = Buffer::from_text(
            "use std::fmt;\n\nfn add(a: i32, b: i32) -> i32 {\n    // somme\n    a + b\n}\n\nstruct P { x: f64 }\n",
        );
        buf.track_edits(true);
        let mut inc = Highlighter::new(Language::Rust).unwrap();
        inc.update(buf.rope(), &[]);
        let mut full = Highlighter::new(Language::Rust).unwrap();

        let mut rng = 0x1234_5678_9abc_def1_u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let snippets = [
            "fn ",
            "let x = 1;",
            "\n",
            "// c\n",
            "\"é😀\"",
            "{",
            "}",
            "(",
            " ",
            "struct S;",
        ];
        for step in 0..150 {
            let n = buf.len_chars();
            if next() % 3 != 0 || n == 0 {
                let at = (next() % (n as u64 + 1)) as usize;
                buf.insert(at, snippets[(next() % snippets.len() as u64) as usize]);
            } else {
                let s = (next() % n as u64) as usize;
                buf.delete(s..(s + (next() % 6) as usize).min(n));
            }
            let edits = buf.take_edits();
            inc.update(buf.rope(), &edits);
            let len = buf.len_bytes();
            let got = inc.highlights(buf.rope(), 0..len);

            full.reset();
            full.update(buf.rope(), &[]);
            let want = full.highlights(buf.rope(), 0..len);
            assert_eq!(got, want, "différence à l'étape {step}");
        }
    }

    #[test]
    fn php_colore_le_code_et_le_html_des_modeles() {
        let text = "<div class=\"a\">\n<?php\n// note\n$x = 42; echo \"salut\";\nfunction f() { return $x; }\n?>\n<p>fin</p>\n</div>\n";
        let s = spans_of(Language::Php, text);
        let has = |t: &str, k: &str| s.iter().any(|(a, b)| a == t && *b == k);
        // le code PHP
        assert!(has("// note", "comment"), "{s:?}");
        assert!(has("42", "number"), "{s:?}");
        assert!(has("function", "keyword"), "{s:?}");
        assert!(has("return", "keyword"), "{s:?}");
        // le HTML qui l'entoure
        assert!(s.iter().any(|(t, k)| t == "div" && *k == "tag"), "{s:?}");
        assert!(s.iter().any(|(t, k)| t == "p" && *k == "tag"), "{s:?}");
        // plages triées et sans chevauchement
        let rope = Rope::from_str(text);
        let mut h = Highlighter::new(Language::Php).unwrap();
        h.update(&rope, &[]);
        let spans = h.highlights(&rope, 0..text.len());
        assert!(spans.windows(2).all(|w| w[0].end <= w[1].start));
    }

    #[test]
    fn php_et_html_suivent_les_modifications() {
        let mut buf = Buffer::from_text("<b>x</b>\n<?php $a = 1; ?>\n");
        buf.track_edits(true);
        let mut h = Highlighter::new(Language::Php).unwrap();
        h.update(buf.rope(), &[]);
        // on ajoute une balise HTML après le code : elle doit être colorée aussi
        let at = buf.len_chars();
        buf.insert(at, "<i>y</i>\n");
        let edits = buf.take_edits();
        h.update(buf.rope(), &edits);
        let text = buf.rope().to_string();
        let spans = h.highlights(buf.rope(), 0..text.len());
        let tags: Vec<&str> = spans
            .iter()
            .filter(|s| s.kind == kind::TAG)
            .map(|s| &text[s.start..s.end])
            .collect();
        assert!(tags.contains(&"i"), "{tags:?}");
        assert!(tags.contains(&"b"), "{tags:?}");
    }

    #[test]
    fn php_sans_html_ni_balises() {
        // un fichier sans aucun texte HTML (seulement `<?php`) ou vide ne plante pas
        let s = spans_of(Language::Php, "<?php\nfunction f() {}\n");
        assert!(
            s.iter().any(|(t, k)| t == "function" && *k == "keyword"),
            "{s:?}"
        );
        let vide = spans_of(Language::Php, "");
        assert!(vide.is_empty());
        let seul = spans_of(Language::Php, "juste du texte <em>HTML</em>");
        assert!(
            seul.iter().any(|(t, k)| t == "em" && *k == "tag"),
            "{seul:?}"
        );
    }

    const PAGE: &str = "<!DOCTYPE html>\n<style>\n  a { color: red; margin: 0 }\n</style>\n<p class=\"x\">hi</p>\n<script>\n  const n = 42; // note\n  function f(s) { return \"t\" + s; }\n</script>\n";

    fn has(s: &[(String, &'static str)], text: &str, kind: &str) -> bool {
        s.iter().any(|(t, k)| t == text && *k == kind)
    }

    #[test]
    fn html_colore_le_javascript_et_le_css_integres() {
        let s = spans_of(Language::Html, PAGE);
        // JavaScript des <script>
        assert!(has(&s, "const", "keyword"), "{s:?}");
        assert!(has(&s, "42", "number"), "{s:?}");
        assert!(has(&s, "// note", "comment"), "{s:?}");
        assert!(has(&s, "function", "keyword"), "{s:?}");
        assert!(has(&s, "\"t\"", "string"), "{s:?}");
        // CSS des <style>
        assert!(has(&s, "color", "property"), "{s:?}");
        assert!(has(&s, "margin", "property"), "{s:?}");
        // et le HTML autour reste coloré
        assert!(has(&s, "script", "tag"), "{s:?}");
        assert!(has(&s, "class", "attribute"), "{s:?}");
    }

    #[test]
    fn php_colore_aussi_le_javascript_et_le_css_des_modeles() {
        let text =
            "<?php $a = 1; ?>\n<style>b { color: blue }</style>\n<script>let z = 7;</script>\n";
        let s = spans_of(Language::Php, text);
        assert!(has(&s, "color", "property"), "{s:?}");
        assert!(has(&s, "let", "keyword"), "{s:?}");
        assert!(has(&s, "7", "number"), "{s:?}");
    }

    #[test]
    fn le_javascript_integre_suit_les_modifications() {
        let mut buf = Buffer::from_text("<p>x</p>\n<script>var a = 1;</script>\n");
        buf.track_edits(true);
        let mut h = Highlighter::new(Language::Html).unwrap();
        h.update(buf.rope(), &[]);
        // on ajoute un script après le premier, puis on édite dedans
        let at = buf.len_chars();
        buf.insert(at, "<script>let b = \"s\";</script>\n");
        let edits = buf.take_edits();
        h.update(buf.rope(), &edits);
        let text = buf.rope().to_string();
        let colors = |h: &mut Highlighter, buf: &Buffer| -> Vec<(String, &'static str)> {
            let t = buf.rope().to_string();
            h.highlights(buf.rope(), 0..t.len())
                .iter()
                .map(|s| (t[s.start..s.end].to_owned(), kind_name(s.kind)))
                .collect()
        };
        let s = colors(&mut h, &buf);
        assert!(has(&s, "var", "keyword"), "{s:?}");
        assert!(has(&s, "let", "keyword"), "{s:?}");
        let at = text.find("\"s\"").unwrap() + 1;
        let char_at = buf.rope().byte_to_char(at);
        buf.insert(char_at, "zz");
        let edits = buf.take_edits();
        h.update(buf.rope(), &edits);
        let s = colors(&mut h, &buf);
        assert!(has(&s, "\"zzs\"", "string"), "{s:?}");
        // le dernier script disparaît : plus de couleur JavaScript pour lui, sans plantage
        let text = buf.rope().to_string();
        let from = text.find("<script>let").unwrap();
        let (a, b) = (
            buf.rope().byte_to_char(from),
            buf.rope().byte_to_char(text.len()),
        );
        buf.delete(a..b);
        let edits = buf.take_edits();
        h.update(buf.rope(), &edits);
        let s = colors(&mut h, &buf);
        assert!(has(&s, "var", "keyword"), "{s:?}");
        assert!(!has(&s, "let", "keyword"), "{s:?}");
    }

    #[test]
    fn les_nouveaux_langages_sont_colores() {
        let md = spans_of(
            Language::Markdown,
            "# Titre\n\nDu *texte* avec `code` et [un lien](http://x.fr).\n\n- item\n\n```\nlet a = 1;\n```\n",
        );
        assert!(has(&md, "Titre", "function"), "{md:?}");
        assert!(has(&md, "code", "string"), "{md:?}");
        assert!(
            has(&md, "*texte*", "type") || has(&md, "texte", "type"),
            "{md:?}"
        );
        assert!(has(&md, "http://x.fr", "attribute"), "{md:?}");
        assert!(has(&md, "- ", "punctuation"), "{md:?}");

        let yaml = spans_of(
            Language::Yaml,
            "# c\nname: demo\nport: 8080\nok: true\nlist:\n  - \"a\"\n",
        );
        assert!(has(&yaml, "# c", "comment"), "{yaml:?}");
        assert!(has(&yaml, "name", "property"), "{yaml:?}");
        assert!(has(&yaml, "8080", "number"), "{yaml:?}");
        assert!(has(&yaml, "\"a\"", "string"), "{yaml:?}");

        let xml = spans_of(
            Language::Xml,
            "<?xml version=\"1.0\"?>\n<!-- c -->\n<root id=\"1\"><item>t</item></root>\n",
        );
        assert!(has(&xml, "<!-- c -->", "comment"), "{xml:?}");
        assert!(has(&xml, "root", "tag"), "{xml:?}");
        assert!(has(&xml, "id", "property"), "{xml:?}");

        let sql = spans_of(
            Language::Sql,
            "-- note\nSELECT id, name FROM users WHERE id = 42 AND name = 'x';\n",
        );
        assert!(has(&sql, "-- note", "comment"), "{sql:?}");
        assert!(has(&sql, "SELECT", "keyword"), "{sql:?}");
        assert!(has(&sql, "42", "number"), "{sql:?}");
        assert!(has(&sql, "'x'", "string"), "{sql:?}");

        let sh = spans_of(
            Language::Shell,
            "#!/bin/bash\n# c\nif [ -f \"$1\" ]; then\n  echo \"ok $HOME\"\nfi\n",
        );
        assert!(has(&sh, "# c", "comment"), "{sh:?}");
        assert!(has(&sh, "if", "keyword"), "{sh:?}");
        assert!(has(&sh, "echo", "function"), "{sh:?}");
    }

    #[test]
    fn markdown_suit_les_modifications() {
        let mut buf = Buffer::from_text("# A\n\ntexte\n");
        buf.track_edits(true);
        let mut h = Highlighter::new(Language::Markdown).unwrap();
        h.update(buf.rope(), &[]);
        let at = buf.len_chars();
        buf.insert(at, "et `code`\n");
        let edits = buf.take_edits();
        h.update(buf.rope(), &edits);
        let t = buf.rope().to_string();
        let s: Vec<_> = h
            .highlights(buf.rope(), 0..t.len())
            .iter()
            .map(|s| (t[s.start..s.end].to_owned(), kind_name(s.kind)))
            .collect();
        assert!(has(&s, "code", "string"), "{s:?}");
        assert!(has(&s, "A", "function"), "{s:?}");
    }

    /// Plages `<script>` / `<style>` connues du highlighter, `(début, fin)`.
    type Spans = Vec<(usize, usize)>;

    fn blocks_of(h: &Highlighter) -> (Spans, Spans) {
        let scan = h.embedded.as_ref().and_then(|e| e.scan.as_ref());
        let list = |f: fn(&Scan) -> &Vec<Block>| {
            scan.map(|s| f(s).iter().map(|b| (b.start, b.end)).collect())
                .unwrap_or_default()
        };
        (list(|s| &s.js), list(|s| &s.css))
    }

    /// Le scan incrémental donne toujours les mêmes plages qu'un scan complet, quelles que soient
    /// les modifications (balises ouvertes, fermées, collées, supprimées).
    #[test]
    fn le_scan_incremental_vaut_le_scan_complet() {
        let pool = [
            "<script>",
            "</script>",
            "<style>",
            "</style>",
            "x",
            "<",
            ">",
            "/",
            "\n",
            "var a = 1;",
            "a { color: red }",
            "<p>",
            "</p>",
            "<?php $a; ?>",
            "\"",
            "<!--",
            "-->",
            "  ",
            "<script>let b;</script>",
            "<style>p{}</style>",
        ];
        for language in [Language::Html, Language::Php] {
            let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
            let mut rnd = |n: usize| {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((seed >> 33) as usize) % n.max(1)
            };
            let mut buf = Buffer::from_text(
                "<p>a</p>\n<script>var z = 1;</script>\n<style>b { margin: 0 }</style>\n<p>c</p>\n",
            );
            buf.track_edits(true);
            let mut h = Highlighter::new(language).unwrap();
            h.update(buf.rope(), &[]);
            for step in 0..120 {
                // un lot de 1 à 3 modifications, comme le ferait un collage ou un annuler groupé
                for _ in 0..=rnd(3) {
                    let len = buf.len_chars();
                    if rnd(3) == 0 && len > 0 {
                        let a = rnd(len);
                        let b = (a + 1 + rnd(12)).min(len);
                        buf.delete(a..b);
                    } else {
                        let at = rnd(len + 1);
                        buf.insert(at, pool[rnd(pool.len())]);
                    }
                }
                let edits = buf.take_edits();
                h.update(buf.rope(), &edits);
                let mut fresh = Highlighter::new(language).unwrap();
                fresh.update(buf.rope(), &[]);
                assert_eq!(
                    blocks_of(&h),
                    blocks_of(&fresh),
                    "{language:?}, pas {step}, texte : {:?}",
                    buf.rope().to_string()
                );
            }
        }
    }

    #[test]
    #[ignore = "mesure"]
    fn mesure_sql() {
        let mut text = String::new();
        while text.len() < 20_000 {
            text.push_str("-- requete\nSELECT u.id, u.name FROM users u JOIN orders o ON o.uid = u.id WHERE u.id = 42 AND o.total > 10.5 ORDER BY u.name;\n");
        }
        let t = std::time::Instant::now();
        let mut h = Highlighter::new(Language::Sql).unwrap();
        let create = t.elapsed();
        let rope = Rope::from_str(&text);
        let t = std::time::Instant::now();
        h.update(&rope, &[]);
        let parse = t.elapsed();
        let t = std::time::Instant::now();
        let n = h.highlights(&rope, 0..text.len().min(8000)).len();
        println!(
            "SQL : creation {:.1} ms, analyse 20 Ko {:.1} ms, fenetre {:.1} ms ({n} plages)",
            create.as_secs_f64() * 1000.0,
            parse.as_secs_f64() * 1000.0,
            t.elapsed().as_secs_f64() * 1000.0
        );
        let t = std::time::Instant::now();
        let _ = Highlighter::new(Language::Rust).unwrap();
        println!(
            "Rust : creation {:.1} ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn html_sans_script_ni_style_ne_cree_pas_de_couche() {
        let mut h = Highlighter::new(Language::Html).unwrap();
        h.update(&Rope::from_str("<p>juste du HTML</p>"), &[]);
        let e = h.embedded.as_mut().unwrap();
        assert!(e.js.is_none() && e.css.is_none());
    }

    /// Coût d'une modification dans un gros fichier HTML plein de `<script>` et de `<style>` :
    /// `cargo test --release -p paulwrite-syntax --lib mesure_html -- --ignored --nocapture`.
    #[test]
    #[ignore = "mesure"]
    fn mesure_html_integre() {
        for kb in [50usize, 200, 1000] {
            let mut text = String::new();
            let mut i = 0;
            while text.len() < kb * 1024 {
                text.push_str(&format!(
                    "<div class=\"c{i}\"><style>.c{i} {{ color: red; margin: {i}px }}</style>
<script>const v{i} = [{i}, \"x\"]; function f{i}(a) {{ return a + {i}; }}</script></div>
"
                ));
                i += 1;
            }
            let mut buf = Buffer::from_text(&text);
            buf.track_edits(true);
            let mut h = Highlighter::new(Language::Html).unwrap();
            let t = std::time::Instant::now();
            h.update(buf.rope(), &[]);
            let full = t.elapsed();
            let mut times = Vec::new();
            for k in 0..20 {
                let at = buf.len_chars() / 2;
                buf.insert(at, if k % 2 == 0 { "x" } else { " " });
                let edits = buf.take_edits();
                let t = std::time::Instant::now();
                h.update(buf.rope(), &edits);
                let r = buf.len_bytes() / 2;
                let _ = h.highlights(buf.rope(), r..r + 8000);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "HTML {kb} Ko : analyse complete {:.0} ms ; modification + fenetre : p50 {:.1} ms, max {:.1} ms",
                full.as_secs_f64() * 1000.0,
                times[times.len() / 2],
                times[times.len() - 1]
            );
        }
    }

    /// Coût d'une modification dans un gros modèle PHP (HTML réanalysé en entier à chaque fois) :
    /// `cargo test --release -p paulwrite-syntax --lib mesure_php -- --ignored --nocapture`.
    #[test]
    #[ignore = "mesure"]
    fn mesure_php() {
        for kb in [50usize, 200, 1000] {
            let mut text = String::new();
            let mut i = 0;
            while text.len() < kb * 1024 {
                text.push_str(&format!(
                    "<div class=\"c{i}\"><?php foreach ($items as $it): ?>
  <a href=\"<?= $it->url ?>\">x{i}</a>
<?php endforeach; // fin ?>
</div>
"
                ));
                i += 1;
            }
            let mut buf = Buffer::from_text(&text);
            buf.track_edits(true);
            let mut h = Highlighter::new(Language::Php).unwrap();
            let t = std::time::Instant::now();
            h.update(buf.rope(), &[]);
            let full = t.elapsed();
            let mut times = Vec::new();
            for k in 0..20 {
                let at = buf.len_chars() / 2;
                buf.insert(at, if k % 2 == 0 { "x" } else { " " });
                let edits = buf.take_edits();
                let t = std::time::Instant::now();
                h.update(buf.rope(), &edits);
                let r = buf.len_bytes() / 2;
                let _ = h.highlights(buf.rope(), r..r + 8000);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "PHP {kb} Ko : analyse complete {:.0} ms ; modification + fenetre : p50 {:.1} ms, max {:.1} ms",
                full.as_secs_f64() * 1000.0,
                times[times.len() / 2],
                times[times.len() - 1]
            );
        }
    }
}
