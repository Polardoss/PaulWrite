//! Arbre syntaxique d'un document et calcul des plages colorées d'une fenêtre de texte.

use std::ops::Range;

use paulwrite_core::TextEdit;
use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, Tree};

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
            "attribute" => kind::ATTRIBUTE,
            "tag" => kind::TAG,
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
    // tampons réutilisés
    captures: Vec<(usize, usize, usize, Kind)>,
    paint: Vec<Kind>,
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
        let query = Query::new(&grammar, language.highlights_query()).map_err(|e| e.to_string())?;
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
        self.tree = self
            .parser
            .parse_with_options(&mut provider, self.tree.as_ref(), None);
    }

    /// Oublie l'arbre (document remplacé) : le prochain `update` sera un parse complet.
    pub fn reset(&mut self) {
        self.tree = None;
        self.parser.reset();
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
            captures,
            paint,
            ..
        } = self;
        captures.clear();
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
                captures.push((
                    m.pattern_index,
                    r.start.max(range.start),
                    r.end.min(range.end),
                    k,
                ));
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
}
