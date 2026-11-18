//! Remplacer dans tout le projet (Ctrl+Maj+H).
//!
//! Se fait **sur le worker**, fichier par fichier, avec le moteur de la recherche dans le document
//! ([`find_in_rope`]) : même syntaxe (casse, mot entier, expression régulière avec `$1`), même
//! lecture et même écriture que l'éditeur ([`io::load_file`], [`io::save_file`] : encodage, BOM et
//! fins de ligne conservés, écriture atomique). Deux passes : un **aperçu** qui compte sans rien
//! écrire (l'utilisateur confirme avec ces chiffres), puis l'application.
//!
//! Les fichiers ouverts **et modifiés** dans l'éditeur ne sont pas touchés (leurs modifications
//! seraient perdues) : ils sont comptés à part. Les autres fichiers ouverts sont rechargés par la
//! détection des modifications faites hors de l'éditeur.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use paulwrite_core::io::{self, LoadOptions};
use paulwrite_search::{find_in_rope, list_files, FindOptions, Found, IndexOptions};
use ropey::Rope;

/// Fichiers plus gros ignorés (octets), comme pour la recherche dans le projet.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Ce qu'on remplace, et où.
#[derive(Clone, Debug)]
pub struct Params {
    pub root: PathBuf,
    pub query: String,
    pub replacement: String,
    pub opts: FindOptions,
    /// Fichiers ouverts avec des modifications non enregistrées : à ne pas toucher.
    pub skip: HashSet<PathBuf>,
}

/// Bilan d'un aperçu ou d'un remplacement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Fichiers où il y a (aperçu) ou eu (remplacement) des occurrences.
    pub files: usize,
    pub occurrences: usize,
    /// Fichiers avec des occurrences mais ouverts et modifiés dans l'éditeur : non touchés.
    pub skipped: usize,
    /// Fichiers illisibles ou non inscriptibles, avec la raison.
    pub errors: Vec<String>,
    /// Le remplacement a été écrit (sinon : aperçu).
    pub applied: bool,
    /// Motif invalide.
    pub error: Option<String>,
}

/// Le texte de `rope` avec les occurrences `found` (triées, disjointes) remplacées.
#[must_use]
pub fn replaced(rope: &Rope, found: &[Found]) -> Rope {
    let mut out = rope.clone();
    // de la fin au début : les positions des occurrences suivantes restent valables
    for f in found.iter().rev() {
        out.remove(f.start..f.end);
        if let Some(text) = &f.replacement {
            out.insert(f.start, text);
        }
    }
    out
}

/// Un fichier qui contient un octet nul n'est pas du texte.
fn looks_binary(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return true;
    };
    let mut head = [0u8; 8192];
    let n = file.read(&mut head).unwrap_or(0);
    // un fichier UTF-16 contient des octets nuls : le BOM le distingue d'un binaire
    let bom = head[..n].starts_with(&[0xFF, 0xFE]) || head[..n].starts_with(&[0xFE, 0xFF]);
    !bom && head[..n].contains(&0)
}

/// Aperçu (`apply == false`) ou remplacement (`apply == true`) dans tous les fichiers du projet
/// (en respectant `.gitignore`). Bloquant. `cancel` interrompt entre deux fichiers.
#[must_use]
pub fn run(params: &Params, apply: bool, cancel: &AtomicBool) -> Report {
    let mut report = Report {
        applied: apply,
        ..Report::default()
    };
    if params.query.is_empty() {
        return report;
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    list_files(&params.root, &IndexOptions::default(), cancel, tx);
    let mut files: Vec<String> = rx.try_iter().flatten().collect();
    files.sort();
    let never = AtomicBool::new(false);
    for rel in files {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let path = params.root.join(&rel);
        match std::fs::metadata(&path) {
            Ok(m) if m.len() <= MAX_FILE_BYTES => {}
            _ => continue,
        }
        if looks_binary(&path) {
            continue;
        }
        let Ok(loaded) = io::load_file(&path, &LoadOptions::default(), None) else {
            continue; // pas du texte lisible (codage inconnu…) : ignoré
        };
        let found = match find_in_rope(
            &loaded.rope,
            &params.query,
            params.opts,
            Some(&params.replacement),
            usize::MAX,
            &never,
        ) {
            Ok(m) => m.found,
            Err(e) => {
                report.error = Some(e);
                return report;
            }
        };
        if found.is_empty() {
            continue;
        }
        if params.skip.contains(&path) {
            report.skipped += 1;
            continue;
        }
        if apply {
            let rope = replaced(&loaded.rope, &found);
            if let Err(e) = io::save_file(&path, &rope, loaded.encoding) {
                report.errors.push(format!("{rel} : {e}"));
                continue;
            }
        }
        report.files += 1;
        report.occurrences += found.len();
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn params(root: &Path, query: &str, replacement: &str, opts: FindOptions) -> Params {
        Params {
            root: root.to_path_buf(),
            query: query.to_owned(),
            replacement: replacement.to_owned(),
            opts,
            skip: HashSet::new(),
        }
    }

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("paulwrite-pr-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    #[test]
    fn remplacer_dans_un_rope() {
        let rope = Rope::from_str("un deux un\nun");
        let found = vec![
            Found {
                start: 0,
                end: 2,
                replacement: Some("1".into()),
            },
            Found {
                start: 8,
                end: 10,
                replacement: Some("111".into()),
            },
            Found {
                start: 11,
                end: 13,
                replacement: Some(String::new()),
            },
        ];
        assert_eq!(replaced(&rope, &found).to_string(), "1 deux 111\n");
        assert_eq!(replaced(&rope, &[]).to_string(), "un deux un\nun");
    }

    #[test]
    fn apercu_puis_remplacement() {
        let dir = project("a");
        fs::write(dir.join("a.txt"), "foo bar foo\r\nfoo\r\n").unwrap();
        fs::write(dir.join("src/b.rs"), "let foo = 1;\n").unwrap();
        fs::write(dir.join("c.txt"), "rien ici\n").unwrap();
        // binaire : jamais touché
        let mut bin = b"foo\0foo".to_vec();
        bin.extend_from_slice(&[0, 1, 2]);
        fs::write(dir.join("d.bin"), &bin).unwrap();
        // ignoré par .gitignore
        fs::write(dir.join(".gitignore"), "ignore/\n").unwrap();
        fs::create_dir_all(dir.join("ignore")).unwrap();
        fs::write(dir.join("ignore/e.txt"), "foo\n").unwrap();

        let p = params(&dir, "foo", "baz", FindOptions::default());
        let never = AtomicBool::new(false);
        let preview = run(&p, false, &never);
        assert_eq!((preview.files, preview.occurrences), (2, 4), "{preview:?}");
        assert!(!preview.applied && preview.errors.is_empty());
        // l'aperçu n'écrit rien
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "foo bar foo\r\nfoo\r\n"
        );

        let done = run(&p, true, &never);
        assert_eq!((done.files, done.occurrences), (2, 4));
        assert!(done.applied);
        // fins de ligne CRLF conservées, binaire et fichier ignoré intacts
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "baz bar baz\r\nbaz\r\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("src/b.rs")).unwrap(),
            "let baz = 1;\n"
        );
        assert_eq!(fs::read(dir.join("d.bin")).unwrap(), bin);
        assert_eq!(
            fs::read_to_string(dir.join("ignore/e.txt")).unwrap(),
            "foo\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn options_regex_et_groupes() {
        let dir = project("b");
        fs::write(dir.join("a.txt"), "id=12 id=7\nFoo foo\n").unwrap();
        let never = AtomicBool::new(false);
        let regex = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        let r = run(&params(&dir, r"id=(\d+)", "n$1", regex), true, &never);
        assert_eq!(r.occurrences, 2);
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "n12 n7\nFoo foo\n"
        );
        // casse : sans majuscule dans le motif, insensible à la casse
        let r = run(
            &params(&dir, "foo", "X", FindOptions::default()),
            true,
            &never,
        );
        assert_eq!(r.occurrences, 2);
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "n12 n7\nX X\n"
        );
        // motif invalide : rapporté, rien n'est écrit
        let r = run(&params(&dir, "(", "x", regex), true, &never);
        assert!(r.error.is_some());
        assert_eq!(
            fs::read_to_string(dir.join("a.txt")).unwrap(),
            "n12 n7\nX X\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fichiers_modifies_dans_l_editeur_et_utf16() {
        let dir = project("c");
        fs::write(dir.join("ouvert.txt"), "cible\n").unwrap();
        fs::write(dir.join("libre.txt"), "cible\n").unwrap();
        // UTF-16 LE avec BOM : encodage conservé
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("cible é\n".encode_utf16().flat_map(u16::to_le_bytes));
        fs::write(dir.join("u16.txt"), &utf16).unwrap();

        let mut p = params(&dir, "cible", "but", FindOptions::default());
        p.skip.insert(dir.join("ouvert.txt"));
        let never = AtomicBool::new(false);
        let r = run(&p, true, &never);
        assert_eq!((r.files, r.occurrences, r.skipped), (2, 2, 1), "{r:?}");
        assert_eq!(
            fs::read_to_string(dir.join("ouvert.txt")).unwrap(),
            "cible\n"
        );
        assert_eq!(fs::read_to_string(dir.join("libre.txt")).unwrap(), "but\n");
        let mut want = vec![0xFF, 0xFE];
        want.extend("but é\n".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(fs::read(dir.join("u16.txt")).unwrap(), want);
        let _ = fs::remove_dir_all(&dir);
    }
}
