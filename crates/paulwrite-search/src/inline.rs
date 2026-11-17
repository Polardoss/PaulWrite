//! Recherche (et remplacement) dans **un** document : toutes les occurrences d'un motif, en
//! positions de caractères. Même moteur que la recherche dans le projet (celui de ripgrep) ;
//! comme elle, la recherche se fait **ligne par ligne** : un motif ne peut pas couvrir un saut de
//! ligne.
//!
//! Bloquant : à appeler depuis un worker. Le texte est lu dans un [`Rope`] (copie O(1) du document),
//! jamais copié en entier.

use std::sync::atomic::{AtomicBool, Ordering};

use grep_matcher::{Captures, Matcher};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use ropey::Rope;

/// Façon de chercher.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    /// Respecter la casse. Sans cela, une majuscule dans le motif la rend quand même sensible.
    pub case_sensitive: bool,
    /// Mot entier seulement.
    pub whole_word: bool,
    /// Le motif est une expression régulière (sinon : texte tel quel).
    pub regex: bool,
}

/// Une occurrence : plage de caractères du document et, pour un remplacement, le texte qui la
/// remplace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub replacement: Option<String>,
}

/// Résultat d'une recherche.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matches {
    pub found: Vec<Found>,
    /// La limite d'occurrences est atteinte : il y en a davantage.
    pub truncated: bool,
    /// La recherche a été annulée avant la fin.
    pub cancelled: bool,
}

fn build(query: &str, opts: FindOptions) -> Result<RegexMatcher, String> {
    // une majuscule dans le motif rend la recherche sensible à la casse (comme Sublime Text)
    let sensitive = opts.case_sensitive || query.chars().any(char::is_uppercase);
    RegexMatcherBuilder::new()
        .case_insensitive(!sensitive)
        .word(opts.whole_word)
        .fixed_strings(!opts.regex)
        .build(query)
        .map_err(|e| e.to_string())
}

/// Cherche toutes les occurrences de `query` dans `rope` (au plus `limit`). Avec `replacement`,
/// chaque occurrence porte le texte de remplacement ; en mode expression régulière, `$1`, `$name`
/// y désignent les groupes capturés. `cancel` interrompt la recherche (vérifié à chaque ligne).
///
/// # Errors
/// Expression régulière invalide.
pub fn find_in_rope(
    rope: &Rope,
    query: &str,
    opts: FindOptions,
    replacement: Option<&str>,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Matches, String> {
    let mut out = Matches::default();
    if query.is_empty() {
        return Ok(out);
    }
    let matcher = build(query, opts)?;
    let mut caps = replacement
        .filter(|_| opts.regex)
        .map(|_| matcher.new_captures().map_err(|e| e.to_string()))
        .transpose()?;
    let mut line = String::new();
    let mut dst: Vec<u8> = Vec::new();
    'lines: for (index, text) in rope.lines().enumerate() {
        if index % 256 == 0 && cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        line.clear();
        for chunk in text.chunks() {
            line.push_str(chunk);
        }
        while line.ends_with(['\n', '\r']) {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }
        let first_char = rope.line_to_char(index);
        let ascii = line.is_ascii();
        let to_chars = |byte: usize| {
            if ascii {
                byte
            } else {
                line[..byte].chars().count()
            }
        };
        let hay = line.as_bytes();
        let mut at = 0;
        while at <= hay.len() {
            let Ok(Some(m)) = matcher.find_at(hay, at) else {
                break;
            };
            // une occurrence vide (expression régulière) avance d'un octet pour ne pas boucler
            if m.end() == m.start() {
                at = m.end() + 1;
                continue;
            }
            if out.found.len() >= limit {
                out.truncated = true;
                break 'lines;
            }
            let rep = replacement.map(|r| match caps.as_mut() {
                Some(c) => {
                    dst.clear();
                    let ok = matcher.captures_at(hay, m.start(), c).unwrap_or(false);
                    if ok {
                        c.interpolate(
                            |name| matcher.capture_index(name),
                            hay,
                            r.as_bytes(),
                            &mut dst,
                        );
                        String::from_utf8_lossy(&dst).into_owned()
                    } else {
                        r.to_owned()
                    }
                }
                None => r.to_owned(),
            });
            out.found.push(Found {
                start: first_char + to_chars(m.start()),
                end: first_char + to_chars(m.end()),
                replacement: rep,
            });
            at = m.end();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, q: &str, opts: FindOptions) -> Vec<(usize, usize)> {
        let m = find_in_rope(
            &Rope::from_str(text),
            q,
            opts,
            None,
            1000,
            &AtomicBool::new(false),
        )
        .unwrap();
        m.found.iter().map(|f| (f.start, f.end)).collect()
    }

    const PLAIN: FindOptions = FindOptions {
        case_sensitive: false,
        whole_word: false,
        regex: false,
    };

    #[test]
    fn texte_simple_sur_plusieurs_lignes() {
        assert_eq!(
            find("foo bar\nfoo\nxfoo", "foo", PLAIN),
            [(0, 3), (8, 11), (13, 16)]
        );
        assert!(find("abc", "", PLAIN).is_empty());
        assert!(find("abc", "zzz", PLAIN).is_empty());
        // pas de chevauchement : « aa » dans « aaaa »
        assert_eq!(find("aaaa", "aa", PLAIN), [(0, 2), (2, 4)]);
    }

    #[test]
    fn casse_intelligente_et_mot_entier() {
        assert_eq!(
            find("Foo foo FOO", "foo", PLAIN).len(),
            3,
            "minuscules : insensible"
        );
        assert_eq!(
            find("Foo foo FOO", "Foo", PLAIN),
            [(0, 3)],
            "une majuscule : sensible"
        );
        let sensitive = FindOptions {
            case_sensitive: true,
            ..PLAIN
        };
        assert_eq!(find("Foo foo", "foo", sensitive), [(4, 7)]);
        let word = FindOptions {
            whole_word: true,
            ..PLAIN
        };
        assert_eq!(
            find("foo foobar barfoo foo", "foo", word),
            [(0, 3), (18, 21)]
        );
    }

    #[test]
    fn expressions_regulieres_et_erreurs() {
        let re = FindOptions {
            regex: true,
            ..PLAIN
        };
        assert_eq!(find("a1 b22 c333", r"\d+", re), [(1, 2), (4, 6), (8, 11)]);
        // sans le mode, les signes spéciaux sont du texte
        assert_eq!(find("a.b axb", "a.b", PLAIN), [(0, 3)]);
        assert!(find_in_rope(
            &Rope::from_str("x"),
            "(",
            re,
            None,
            10,
            &AtomicBool::new(false)
        )
        .is_err());
        // les occurrences vides ne bouclent pas
        assert_eq!(find("abc", "x*", re), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn positions_en_caracteres_avec_accents_et_emoji() {
        assert_eq!(find("é😀 foo\né foo", "foo", PLAIN), [(3, 6), (9, 12)]);
        // fins de ligne CRLF
        assert_eq!(find("a\r\nfoo\r\n", "foo", PLAIN), [(3, 6)]);
    }

    #[test]
    fn remplacements_texte_et_groupes() {
        let run = |t: &str, q: &str, o: FindOptions, r: &str| {
            find_in_rope(
                &Rope::from_str(t),
                q,
                o,
                Some(r),
                100,
                &AtomicBool::new(false),
            )
            .unwrap()
            .found
            .into_iter()
            .map(|f| f.replacement.unwrap())
            .collect::<Vec<_>>()
        };
        assert_eq!(
            run("a b a", "a", PLAIN, "$1"),
            ["$1", "$1"],
            "texte : tel quel"
        );
        let re = FindOptions {
            regex: true,
            ..PLAIN
        };
        assert_eq!(run("k=1 z=22", r"(\w)=(\d+)", re, "$2:$1"), ["1:k", "22:z"]);
        assert_eq!(run("ab", "(?P<x>a)", re, "[${x}]"), ["[a]"]);
    }

    #[test]
    fn limite_et_annulation() {
        let text = "x\n".repeat(50);
        let m = find_in_rope(
            &Rope::from_str(&text),
            "x",
            PLAIN,
            None,
            10,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(m.found.len(), 10);
        assert!(m.truncated && !m.cancelled);
        // annulée d'entrée : aucune occurrence, signalée
        let big = "x\n".repeat(1000);
        let m = find_in_rope(
            &Rope::from_str(&big),
            "x",
            PLAIN,
            None,
            10_000,
            &AtomicBool::new(true),
        )
        .unwrap();
        assert!(m.cancelled);
        assert!(m.found.len() < 1000);
    }
}
