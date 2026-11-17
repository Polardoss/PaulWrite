//! Conversions chemin Windows <-> URI `file://` du protocole LSP.

use std::path::{Path, PathBuf};

/// URI `file:///C:/dossier/fichier.rs` d'un chemin (caractères non ASCII et espaces encodés).
#[must_use]
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Chemin d'une URI `file://` (`None` si ce n'est pas une URI de fichier). Gère `file:///C:/…`,
/// `file:///c%3A/…` (lettre de lecteur encodée, comme le fait rust-analyzer) et les espaces.
#[must_use]
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = String::from_utf8(percent_decode(rest)).ok()?;
    let mut s = decoded.as_str();
    // `/C:/dir` -> `C:/dir` ; un chemin Unix `/dir` reste tel quel
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        s = &s[1..];
    }
    Some(PathBuf::from(if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s.to_owned()
    }))
}

/// Clé de comparaison d'un chemin : séparateurs `/`, minuscules (Windows ne distingue pas la
/// casse, et les serveurs renvoient parfois `c:` pour `C:`).
#[must_use]
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chemin_windows_vers_uri_et_retour() {
        let p = Path::new(r"C:\Users\Moi\Mon Projet\é.rs");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///C:/Users/Moi/Mon%20Projet/%C3%A9.rs");
        assert_eq!(
            uri_to_path(&uri).unwrap(),
            PathBuf::from(if cfg!(windows) {
                r"C:\Users\Moi\Mon Projet\é.rs"
            } else {
                "C:/Users/Moi/Mon Projet/é.rs"
            })
        );
    }

    #[test]
    fn lettre_de_lecteur_encodee_par_rust_analyzer() {
        let p = uri_to_path("file:///c%3A/Users/Moi/src/main.rs").unwrap();
        assert_eq!(path_key(&p), "c:/users/moi/src/main.rs");
        assert_eq!(
            path_key(&uri_to_path("file:///C:/Users/Moi/src/main.rs").unwrap()),
            "c:/users/moi/src/main.rs"
        );
    }

    #[test]
    fn uri_invalide_ou_non_fichier() {
        assert_eq!(uri_to_path("http://exemple.fr/a"), None);
        assert_eq!(uri_to_path("untitled:Sans-titre-1"), None);
        // pourcentage mal formé : conservé tel quel, sans panique
        assert!(uri_to_path("file:///C:/a%zz/b%").is_some());
    }

    #[test]
    fn cle_de_comparaison() {
        assert_eq!(
            path_key(Path::new(r"C:\Dossier\Fichier.RS")),
            path_key(Path::new("c:/dossier/fichier.rs"))
        );
    }
}
