//! Faux serveur de langage pour les tests : il parle réellement LSP (entrée/sortie standard) de
//! façon déterministe. Il **reconstruit le texte des documents** à partir des modifications reçues,
//! ce qui permet de vérifier que le client synchronise correctement (plages en UTF-16 comprises).
//!
//! Options : `--sync full|incremental|none`, `--delay-ms N` (retarde les réponses aux requêtes),
//! `--exit-on-completion` (s'arrête sans répondre à la première complétion).
//!
//! Comportement : un diagnostic d'erreur sur chaque occurrence de `ERROR`, d'avertissement sur
//! `WARN` ; survol = texte de la ligne survolée *telle que le serveur la connaît* ; complétion =
//! éléments fixes dont un qui rappelle la position reçue ; définition = première ligne contenant
//! `fn <mot sous le curseur>`.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::time::Duration;

use paulwrite_lsp::{read_message, write_message};
use serde_json::{json, Value};

/// Décalage en octets de la position `(ligne, colonne UTF-16)` dans `text`.
fn byte_offset(text: &str, line: usize, col16: usize) -> usize {
    let mut offset = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i == line {
            let mut units = 0;
            for (b, c) in l.char_indices() {
                if units >= col16 {
                    return offset + b;
                }
                units += c.len_utf16();
            }
            return offset + l.len();
        }
        offset += l.len() + 1;
    }
    text.len()
}

fn pos(v: &Value) -> (usize, usize) {
    (
        v["line"].as_u64().unwrap_or(0) as usize,
        v["character"].as_u64().unwrap_or(0) as usize,
    )
}

fn utf16_col(line: &str, byte: usize) -> usize {
    line[..byte].chars().map(char::len_utf16).sum()
}

fn diagnostics(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (n, line) in text.split('\n').enumerate() {
        for (word, severity) in [("ERROR", 1), ("WARN", 2)] {
            for (b, _) in line.match_indices(word) {
                let c0 = utf16_col(line, b);
                out.push(json!({
                    "range": {"start": {"line": n, "character": c0},
                              "end": {"line": n, "character": c0 + word.len()}},
                    "severity": severity,
                    "message": format!("{word} trouvé à la ligne {n}")
                }));
            }
        }
    }
    out
}

fn word_at(line: &str, col16: usize) -> String {
    let mut units = 0;
    let mut idx = line.len();
    for (b, c) in line.char_indices() {
        if units >= col16 {
            idx = b;
            break;
        }
        units += c.len_utf16();
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = line[..idx].rfind(|c| !is_word(c)).map_or(0, |i| {
        i + line[i..].chars().next().map_or(1, char::len_utf8)
    });
    let end = line[idx..]
        .find(|c| !is_word(c))
        .map_or(line.len(), |i| idx + i);
    line[start..end].to_owned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name);
    let sync = flag("--sync")
        .and_then(|i| args.get(i + 1))
        .map_or("incremental", String::as_str)
        .to_owned();
    let delay = flag("--delay-ms")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let exit_on_completion = flag("--exit-on-completion").is_some();

    let mut reader = BufReader::new(io::stdin());
    let mut out = io::stdout();
    let mut docs: HashMap<String, String> = HashMap::new();
    let mut config_ok = false;

    let publish = |out: &mut io::Stdout, uri: &str, text: &str| {
        let _ = write_message(
            out,
            &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                    "params": {"uri": uri, "diagnostics": diagnostics(text)}}),
        );
    };

    while let Ok(Some(msg)) = read_message(&mut reader) {
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some("initialize"), Some(id)) => {
                let change = match sync.as_str() {
                    "full" => 1,
                    "none" => 0,
                    _ => 2,
                };
                let _ = write_message(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": {"capabilities": {
                        "textDocumentSync": {"openClose": true, "change": change},
                        "completionProvider": {}, "hoverProvider": true, "definitionProvider": true
                    }}}),
                );
            }
            (Some("initialized"), None) => {
                // requête du serveur vers le client : le client doit y répondre
                let _ = write_message(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": 900, "method": "workspace/configuration",
                            "params": {"items": [{"section": "a"}, {"section": "b"}]}}),
                );
            }
            (None, Some(id)) if id == json!(900) => {
                config_ok = msg["result"].as_array().is_some_and(|a| a.len() == 2);
            }
            (Some("textDocument/didOpen"), None) => {
                let uri = params["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                let text = params["textDocument"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                publish(&mut out, &uri, &text);
                docs.insert(uri, text);
            }
            (Some("textDocument/didChange"), None) => {
                let uri = params["textDocument"]["uri"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                let text = docs.entry(uri.clone()).or_default();
                for change in params["contentChanges"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    if let Some(range) = change.get("range") {
                        let a = byte_offset(text, pos(&range["start"]).0, pos(&range["start"]).1);
                        let b = byte_offset(text, pos(&range["end"]).0, pos(&range["end"]).1);
                        text.replace_range(a..b.max(a), change["text"].as_str().unwrap_or(""));
                    } else {
                        *text = change["text"].as_str().unwrap_or("").to_owned();
                    }
                }
                let snapshot = text.clone();
                publish(&mut out, &uri, &snapshot);
            }
            (Some("textDocument/didClose"), None) => {
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    docs.remove(uri);
                }
            }
            (
                Some(
                    m @ ("textDocument/completion"
                    | "textDocument/hover"
                    | "textDocument/definition"),
                ),
                Some(id),
            ) => {
                if exit_on_completion && m == "textDocument/completion" {
                    std::process::exit(1);
                }
                if delay > 0 {
                    std::thread::sleep(Duration::from_millis(delay));
                }
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                let (line, col) = pos(&params["position"]);
                let text = docs.get(uri).cloned().unwrap_or_default();
                let line_text = text.split('\n').nth(line).unwrap_or("").to_owned();
                let result = match m {
                    "textDocument/completion" => json!({"items": [
                        {"label": "alpha"},
                        {"label": "beta", "insertText": "beta()", "detail": "fonction"},
                        {"label": format!("pos:{line}:{col}")}
                    ]}),
                    "textDocument/hover" => json!({"contents": {
                        "kind": "plaintext",
                        "value": format!("ligne {line}: {line_text}{}", if config_ok { " [cfg ok]" } else { "" })
                    }}),
                    _ => {
                        let word = word_at(&line_text, col);
                        let needle = format!("fn {word}");
                        text.split('\n')
                            .enumerate()
                            .find_map(|(n, l)| l.find(&needle).map(|b| (n, l, b + 3)))
                            .map_or(Value::Null, |(n, l, b)| {
                                let c = utf16_col(l, b);
                                json!({"uri": uri, "range": {
                                    "start": {"line": n, "character": c},
                                    "end": {"line": n, "character": c + word.len()}}})
                            })
                    }
                };
                let _ = write_message(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": result}),
                );
            }
            (Some("shutdown"), Some(id)) => {
                let _ = write_message(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": id, "result": null}),
                );
            }
            (Some("exit"), None) => return,
            _ => {}
        }
    }
}
