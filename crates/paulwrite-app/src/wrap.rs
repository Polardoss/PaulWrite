//! Retour à la ligne automatique : découpe d'une ligne en segments (rangées affichées).
//!
//! La police est à chasse fixe : la largeur se compte en colonnes. Une tabulation avance au
//! prochain multiple de la largeur de tabulation (4 par défaut), **à partir du début du segment** (le moteur de rendu pose
//! chaque rangée comme un texte autonome, ses taquets de tabulation partent donc de son début).

/// Largeur en colonnes de `text` posé au début d'une rangée.
#[must_use]
pub fn visual_width<I: IntoIterator<Item = char>>(text: I) -> usize {
    let mut col = 0;
    for c in text {
        col = advance(col, c);
    }
    col
}

fn advance(col: usize, c: char) -> usize {
    if c == '\t' {
        let tab = crate::editing::tab_width();
        (col / tab + 1) * tab
    } else {
        col + 1
    }
}

/// Un retour à la ligne est permis après ce caractère (espace, tiret, barre).
fn allows_break_after(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '/' | '\\')
}

/// Ajoute à `out` les positions (en caractères depuis le début de `text`) où commencent les
/// segments **suivants** du premier : `out` reste vide si `text` tient sur une rangée de `cols`
/// colonnes.
///
/// On coupe après le dernier espace (ou tiret, barre) qui précède le débordement ; sans
/// occasion de couper, au caractère qui déborde. Les espaces de fin de segment peuvent dépasser
/// la largeur (ils restent à la fin de la rangée, jamais au début de la suivante).
pub fn breaks(text: &str, cols: usize, out: &mut Vec<usize>) {
    let cols = cols.max(1);
    let mut start = 0usize; // début du segment courant (en caractères)
    let mut col = 0usize; // colonnes déjà occupées dans le segment
                          // dernière occasion de couper : (position en caractères, position en octets)
    let mut candidate: Option<(usize, usize)> = None;
    // le segment contient déjà autre chose que de l'indentation (on ne coupe pas dedans)
    let mut seen_text = false;
    for (i, (byte, c)) in text.char_indices().enumerate() {
        // un espace qui déborde reste sur la rangée courante : la coupure viendra après lui
        if col >= cols && !c.is_whitespace() && i > start {
            let (at, at_byte) = match candidate {
                Some((b, bb)) if b > start => (b, bb),
                _ => (i, byte),
            };
            out.push(at);
            start = at;
            col = visual_width(text[at_byte..byte].chars());
            seen_text = text[at_byte..byte].chars().any(|c| !c.is_whitespace());
            candidate = None;
            // le caractère qui déborde a pu devenir le suivant d'une occasion de couper
            if col >= cols && at != i {
                // après la coupure, le reste du texte déjà lu tient mal : on coupe encore au
                // caractère courant (cas d'un mot plus long que la rangée)
                out.push(i);
                start = i;
                col = 0;
                seen_text = false;
            }
        }
        col = advance(col, c);
        if !c.is_whitespace() {
            seen_text = true;
        }
        if seen_text && allows_break_after(c) {
            candidate = Some((i + 1, byte + c.len_utf8()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuts(text: &str, cols: usize) -> Vec<usize> {
        let mut v = Vec::new();
        breaks(text, cols, &mut v);
        v
    }

    /// Les segments obtenus, recollés, redonnent le texte.
    fn segments(text: &str, cols: usize) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut starts = vec![0];
        starts.extend(cuts(text, cols));
        starts.push(chars.len());
        starts
            .windows(2)
            .map(|w| chars[w[0]..w[1]].iter().collect())
            .collect()
    }

    #[test]
    fn une_ligne_courte_ne_se_coupe_pas() {
        assert!(cuts("", 10).is_empty());
        assert!(cuts("abc", 10).is_empty());
        assert!(cuts("0123456789", 10).is_empty(), "pile la largeur");
    }

    #[test]
    fn coupe_apres_les_espaces() {
        assert_eq!(
            segments("hello world foo", 8),
            ["hello ", "world ", "foo"].map(String::from)
        );
    }

    #[test]
    fn un_mot_trop_long_est_coupe_au_caractere() {
        assert_eq!(
            segments("abcdefghij", 4),
            ["abcd", "efgh", "ij"].map(String::from)
        );
        // un mot long après un mot court : on coupe d'abord après l'espace
        assert_eq!(
            segments("ab cdefghijkl", 5),
            ["ab ", "cdefg", "hijkl"].map(String::from)
        );
    }

    #[test]
    fn les_tirets_et_barres_permettent_de_couper() {
        assert_eq!(
            segments("a/b/c/d/e/f", 6),
            ["a/b/c/", "d/e/f"].map(String::from)
        );
        assert_eq!(
            segments("well-known-fact", 11),
            ["well-known-", "fact"].map(String::from)
        );
    }

    #[test]
    fn les_espaces_en_fin_de_rangee_restent_sur_elle() {
        // 10 espaces dépassent la largeur 4 : ils restent ensemble, le mot passe à la suite
        let s = segments("ab          cd", 4);
        assert_eq!(s, ["ab          ", "cd"].map(String::from));
        assert!(s.iter().skip(1).all(|r| !r.starts_with(' ')));
    }

    #[test]
    fn les_tabulations_comptent_comme_les_taquets_du_rendu() {
        // « \tab » : 4 colonnes pour la tabulation puis a, b ; largeur 6 : tient
        assert!(cuts("\tab", 6).is_empty());
        // « \tabcd » : 4 + 4 = 8 colonnes > 6 : coupe avant « cd »... faute d'espace, au débordement
        assert_eq!(segments("\tabcd", 6), ["\tab", "cd"].map(String::from));
        assert_eq!(visual_width("a\tb".chars()), 5);
        assert_eq!(visual_width("\t\t".chars()), 8);
    }

    #[test]
    fn les_caracteres_multi_octets_sont_comptes_en_caracteres() {
        let s = segments("éééééé ààààà", 6);
        assert_eq!(s, ["éééééé ", "ààààà"].map(String::from));
        let s = segments("😀😀😀😀😀", 2);
        assert_eq!(s, ["😀😀", "😀😀", "😀"].map(String::from));
    }

    #[test]
    fn jamais_de_segment_vide_ni_de_boucle() {
        for cols in 1..12 {
            for text in [
                "a",
                "aa bb cc dd ee",
                "         ",
                "a b c d e f g h",
                "\t\t\tx",
                "xxxxxxxxxxxxxxxxxxxx",
            ] {
                let seg = segments(text, cols);
                assert!(
                    seg.iter().all(|s| !s.is_empty()),
                    "{text:?} cols={cols} : {seg:?}"
                );
                assert_eq!(seg.concat(), text);
            }
        }
    }

    #[test]
    fn largeur_zero_ou_un() {
        assert_eq!(segments("abc", 0).concat(), "abc");
        assert_eq!(segments("abc", 1), ["a", "b", "c"].map(String::from));
    }
}
