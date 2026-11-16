//! Chargement (mmap ou flux) et sauvegarde atomique de fichiers.
//!
//! Ces fonctions sont **bloquantes** et prévues pour tourner sur un worker : le thread UI ne fait
//! jamais d'I/O. Le callback de progression permet d'afficher l'avancement et d'annuler.
//!
//! # mmap
//! Les fichiers de plus de [`LoadOptions::mmap_threshold`] octets sont lus via un mapping mémoire
//! (pas de copie intermédiaire dans le tampon de lecture). Risque connu : si un autre processus
//! **tronque** le fichier pendant le chargement, l'accès au mapping provoque une exception système.
//! Le mapping ne vit que le temps du chargement, ce qui limite la fenêtre d'exposition.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ropey::{Rope, RopeBuilder};

use crate::encoding::{Decoder, Encoding, DETECT_SAMPLE};
use crate::error::{Error, Result};
use crate::line_ending::{LineEnding, Sniffer};

/// Callback de progression : `(octets_lus, total)`. Rendre `false` annule l'opération.
pub type ProgressFn<'a> = &'a mut dyn FnMut(u64, u64) -> bool;

/// Réglages de chargement.
#[derive(Clone, Copy, Debug)]
pub struct LoadOptions {
    /// Taille (octets) à partir de laquelle on utilise mmap plutôt qu'une lecture en flux.
    pub mmap_threshold: u64,
    /// Taille des morceaux traités entre deux appels de progression.
    pub chunk_size: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            mmap_threshold: 8 * 1024 * 1024,
            chunk_size: 1024 * 1024,
        }
    }
}

/// Résultat d'un chargement.
#[derive(Debug)]
pub struct LoadedText {
    pub rope: Rope,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
}

/// Construit un rope à partir d'octets fournis par morceaux.
struct Assembler {
    builder: RopeBuilder,
    decoder: Decoder,
    encoding: Encoding,
    sniffer: Sniffer,
}

impl Assembler {
    fn new(encoding: Encoding) -> Self {
        Self {
            builder: RopeBuilder::new(),
            decoder: Decoder::new(encoding.kind),
            encoding,
            sniffer: Sniffer::default(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        let Self {
            builder,
            decoder,
            sniffer,
            ..
        } = self;
        decoder.feed(bytes, &mut |s| {
            sniffer.feed(s.as_bytes());
            builder.append(s);
        })
    }

    fn finish(self) -> Result<LoadedText> {
        self.decoder.finish()?;
        Ok(LoadedText {
            rope: self.builder.finish(),
            encoding: self.encoding,
            line_ending: self.sniffer.result(),
        })
    }
}

/// Charge un fichier : mmap au-delà du seuil, lecture en flux en dessous.
///
/// # Errors
/// Erreur d'E/S, encodage non pris en charge ou invalide, ou annulation.
pub fn load_file(
    path: &Path,
    opts: &LoadOptions,
    progress: Option<ProgressFn<'_>>,
) -> Result<LoadedText> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(LoadedText {
            rope: Rope::new(),
            encoding: Encoding::UTF8,
            line_ending: LineEnding::default(),
        });
    }
    if len >= opts.mmap_threshold {
        load_mmap(&file, len, opts, progress)
    } else {
        load_reader(file, Some(len), opts, progress)
    }
}

#[allow(unsafe_code)] // seul usage d'`unsafe` du crate : voir la note « mmap » en tête de module
fn map(file: &File) -> std::io::Result<memmap2::Mmap> {
    // SAFETY: le mapping est en lecture seule et ne vit que le temps du chargement. Une
    // troncature concurrente par un autre processus est le risque documenté du module.
    unsafe { memmap2::Mmap::map(file) }
}

fn load_mmap(
    file: &File,
    len: u64,
    opts: &LoadOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<LoadedText> {
    let mmap = map(file)?;
    load_slice(&mmap, len, opts, &mut progress)
}

/// Charge un contenu déjà en mémoire (mmap ou tampon).
fn load_slice(
    bytes: &[u8],
    total: u64,
    opts: &LoadOptions,
    progress: &mut Option<ProgressFn<'_>>,
) -> Result<LoadedText> {
    let encoding = Encoding::detect(&bytes[..bytes.len().min(DETECT_SAMPLE)])?;
    let mut asm = Assembler::new(encoding);
    let body = &bytes[encoding.bom_bytes().len()..];
    let mut done = encoding.bom_bytes().len() as u64;
    for chunk in body.chunks(opts.chunk_size.max(1)) {
        asm.feed(chunk)?;
        done += chunk.len() as u64;
        if let Some(p) = progress.as_mut() {
            if !p(done, total) {
                return Err(Error::Cancelled);
            }
        }
    }
    asm.finish()
}

/// Charge depuis un flux (fichier, pipe...). `total` : taille connue, pour la progression.
///
/// # Errors
/// Erreur d'E/S, encodage non pris en charge ou invalide, ou annulation.
pub fn load_reader<R: Read>(
    mut reader: R,
    total: Option<u64>,
    opts: &LoadOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<LoadedText> {
    let mut buf = vec![0u8; opts.chunk_size.max(DETECT_SAMPLE)];
    let mut filled = read_full(&mut reader, &mut buf)?;
    let encoding = Encoding::detect(&buf[..filled.min(DETECT_SAMPLE)])?;
    let mut asm = Assembler::new(encoding);
    let mut done = 0u64;
    let mut start = encoding.bom_bytes().len().min(filled);
    let total = total.unwrap_or(0);
    loop {
        if filled > start {
            asm.feed(&buf[start..filled])?;
        }
        done += filled as u64;
        if let Some(p) = progress.as_mut() {
            if !p(done, total.max(done)) {
                return Err(Error::Cancelled);
            }
        }
        if filled < buf.len() {
            break; // fin du flux
        }
        start = 0;
        filled = read_full(&mut reader, &mut buf)?;
        if filled == 0 {
            break;
        }
    }
    asm.finish()
}

/// Lit jusqu'à remplir `buf` ou atteindre la fin du flux ; rend le nombre d'octets lus.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path_for(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = format!(".{name}.paulwrite-{}-{n}.tmp", std::process::id());
    target.with_file_name(tmp)
}

/// Écrit le rope dans `writer` avec l'encodage donné (BOM compris), en flux.
///
/// # Errors
/// Erreur d'E/S.
pub fn write_rope<W: Write>(writer: W, rope: &Rope, encoding: Encoding) -> Result<()> {
    let mut w = BufWriter::with_capacity(1024 * 1024, writer);
    w.write_all(encoding.bom_bytes())?;
    match encoding.kind {
        crate::encoding::EncodingKind::Utf8 => {
            for chunk in rope.chunks() {
                w.write_all(chunk.as_bytes())?;
            }
        }
        _ => {
            let mut scratch = Vec::with_capacity(8 * 1024);
            for chunk in rope.chunks() {
                scratch.clear();
                encoding.encode_into(chunk, &mut scratch);
                w.write_all(&scratch)?;
            }
        }
    }
    w.flush()?;
    Ok(())
}

/// Sauvegarde **atomique** : écriture dans un fichier temporaire du même dossier, `fsync`, puis
/// renommage par-dessus la cible (remplacement natif sous Windows). En cas d'échec, le fichier
/// d'origine reste intact et le temporaire est supprimé.
///
/// # Errors
/// Erreur d'E/S.
pub fn save_file(path: &Path, rope: &Rope, encoding: Encoding) -> Result<()> {
    let tmp = temp_path_for(path);
    let result = (|| -> Result<()> {
        let file = File::create(&tmp)?;
        write_rope(&file, rope, encoding)?;
        file.sync_all()?;
        drop(file);
        // Conserve les permissions (attribut lecture seule) du fichier remplacé.
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::EncodingKind;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn small_chunks(n: usize) -> LoadOptions {
        LoadOptions {
            mmap_threshold: 0,
            chunk_size: n,
        }
    }

    #[test]
    fn roundtrip_all_encodings_with_tiny_chunks() {
        let dir = tmp_dir();
        let text = "héllo\r\nwörld € 😀\r\nfin";
        for enc in [
            Encoding::UTF8,
            Encoding::UTF8_BOM,
            Encoding::UTF16_LE_BOM,
            Encoding::UTF16_BE_BOM,
        ] {
            let path = dir.path().join("f.txt");
            save_file(&path, &Rope::from_str(text), enc).unwrap();
            for chunk in [1, 2, 3, 5, 7, 4096] {
                let loaded = load_file(&path, &small_chunks(chunk), None).unwrap();
                assert_eq!(loaded.rope.to_string(), text, "{enc:?} chunk={chunk}");
                assert_eq!(loaded.encoding, enc);
                assert_eq!(loaded.line_ending, LineEnding::CrLf);
            }
        }
    }

    #[test]
    fn stream_and_mmap_paths_agree() {
        let dir = tmp_dir();
        let path = dir.path().join("f.txt");
        let text = "a\nb\nc é\n".repeat(1000);
        fs::write(&path, &text).unwrap();
        let via_mmap = load_file(
            &path,
            &LoadOptions {
                mmap_threshold: 0,
                chunk_size: 64,
            },
            None,
        )
        .unwrap();
        let via_stream = load_file(
            &path,
            &LoadOptions {
                mmap_threshold: u64::MAX,
                chunk_size: 64,
            },
            None,
        )
        .unwrap();
        assert_eq!(via_mmap.rope.to_string(), text);
        assert_eq!(via_stream.rope.to_string(), text);
    }

    #[test]
    fn crlf_split_across_chunks_is_one_line_break() {
        let dir = tmp_dir();
        let path = dir.path().join("f.txt");
        fs::write(&path, "ab\r\ncd\r\n").unwrap();
        for chunk in 1..=9 {
            let l = load_file(&path, &small_chunks(chunk), None).unwrap();
            assert_eq!(l.rope.len_lines(), 3, "chunk={chunk}");
        }
    }

    #[test]
    fn empty_file_and_missing_trailing_newline() {
        let dir = tmp_dir();
        let path = dir.path().join("e.txt");
        fs::write(&path, "").unwrap();
        let l = load_file(&path, &LoadOptions::default(), None).unwrap();
        assert_eq!(l.rope.len_chars(), 0);
        fs::write(&path, "no newline").unwrap();
        let l = load_file(&path, &LoadOptions::default(), None).unwrap();
        assert_eq!(l.rope.to_string(), "no newline");
    }

    #[test]
    fn utf16_without_bom_is_detected_for_ascii_text() {
        let dir = tmp_dir();
        let path = dir.path().join("u.txt");
        let enc = Encoding {
            kind: EncodingKind::Utf16Le,
            bom: false,
        };
        save_file(&path, &Rope::from_str("plain ascii text\nsecond line"), enc).unwrap();
        let l = load_file(&path, &LoadOptions::default(), None).unwrap();
        assert_eq!(l.encoding, enc);
        assert_eq!(l.rope.to_string(), "plain ascii text\nsecond line");
    }

    #[test]
    fn invalid_files_are_rejected_not_corrupted() {
        let dir = tmp_dir();
        let path = dir.path().join("l1.txt");
        fs::write(&path, b"caf\xE9 au lait").unwrap();
        assert!(matches!(
            load_file(&path, &LoadOptions::default(), None),
            Err(Error::UnsupportedEncoding)
        ));
        // UTF-8 valide au début, invalide plus loin (au-delà de l'échantillon de détection).
        let mut bytes = vec![b'a'; DETECT_SAMPLE + 10];
        bytes.push(0xFF);
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            load_file(&path, &small_chunks(1000), None),
            Err(Error::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn progress_can_cancel() {
        let dir = tmp_dir();
        let path = dir.path().join("p.txt");
        fs::write(&path, "x".repeat(10_000)).unwrap();
        let mut calls = 0;
        let mut cb = |done: u64, total: u64| {
            calls += 1;
            assert!(done <= total);
            calls < 3
        };
        let r = load_file(&path, &small_chunks(100), Some(&mut cb));
        assert!(matches!(r, Err(Error::Cancelled)));
    }

    #[test]
    fn atomic_save_replaces_and_leaves_no_temp() {
        let dir = tmp_dir();
        let path = dir.path().join("s.txt");
        fs::write(&path, "old").unwrap();
        save_file(&path, &Rope::from_str("new content"), Encoding::UTF8).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["s.txt"]);
    }

    #[test]
    fn failed_save_keeps_original_and_cleans_up() {
        let dir = tmp_dir();
        // La cible est un dossier : le renommage échoue, le fichier temporaire doit disparaître.
        let target = dir.path().join("sub");
        fs::create_dir(&target).unwrap();
        assert!(save_file(&target, &Rope::from_str("x"), Encoding::UTF8).is_err());
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["sub"]);
    }
}
