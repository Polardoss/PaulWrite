//! Transport JSON-RPC du protocole LSP : messages encadrés par `Content-Length: N\r\n\r\n`.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Taille maximale acceptée pour un message (garde-fou contre un serveur défaillant).
const MAX_MESSAGE: usize = 256 * 1024 * 1024;

/// Écrit un message encadré.
///
/// # Errors
/// Erreur d'écriture (serveur arrêté, tuyau fermé).
pub fn write_message(w: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

/// Lit le message suivant. `Ok(None)` à la fin du flux (le serveur s'est arrêté).
///
/// # Errors
/// En-tête invalide, corps tronqué ou JSON invalide.
pub fn read_message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue; // ligne vide parasite avant un en-tête
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Content-Length invalide")
                })?);
            }
            // Content-Type et les autres en-têtes sont ignorés.
        }
    }
    let len = length.unwrap_or(0);
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message trop grand",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Read};

    #[test]
    fn aller_retour_avec_accents() {
        let mut buf = Vec::new();
        write_message(&mut buf, &json!({"a": "é😀", "n": 3})).unwrap();
        write_message(&mut buf, &json!({"b": [1, 2]})).unwrap();
        assert!(buf.starts_with(b"Content-Length: "));
        let mut r = Cursor::new(buf);
        assert_eq!(
            read_message(&mut r).unwrap().unwrap(),
            json!({"a": "é😀", "n": 3})
        );
        assert_eq!(read_message(&mut r).unwrap().unwrap(), json!({"b": [1, 2]}));
        assert!(read_message(&mut r).unwrap().is_none(), "fin du flux");
    }

    #[test]
    fn autres_en_tetes_et_casse_ignores() {
        let body = br#"{"ok":true}"#;
        let mut data = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        )
        .into_bytes();
        data.extend_from_slice(body);
        let v = read_message(&mut Cursor::new(data)).unwrap().unwrap();
        assert_eq!(v, json!({"ok": true}));
    }

    #[test]
    fn messages_invalides() {
        // JSON invalide
        let bad = b"Content-Length: 3\r\n\r\n{x}".to_vec();
        assert!(read_message(&mut Cursor::new(bad)).is_err());
        // corps tronqué
        let short = b"Content-Length: 50\r\n\r\n{}".to_vec();
        assert!(read_message(&mut Cursor::new(short)).is_err());
        // longueur absurde
        let huge = b"Content-Length: 99999999999\r\n\r\n".to_vec();
        assert!(read_message(&mut Cursor::new(huge)).is_err());
        let not_num = b"Content-Length: abc\r\n\r\n".to_vec();
        assert!(read_message(&mut Cursor::new(not_num)).is_err());
    }

    #[test]
    fn lecture_par_petits_morceaux() {
        // un lecteur qui ne rend qu'un octet à la fois ne doit rien casser
        struct OneByte(Cursor<Vec<u8>>);
        impl Read for OneByte {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = buf.len().min(1);
                self.0.read(&mut buf[..n])
            }
        }
        let mut data = Vec::new();
        write_message(&mut data, &json!({"x": "ligne\nsuivante"})).unwrap();
        let mut r = io::BufReader::with_capacity(1, OneByte(Cursor::new(data)));
        assert_eq!(
            read_message(&mut r).unwrap().unwrap(),
            json!({"x": "ligne\nsuivante"})
        );
    }
}
