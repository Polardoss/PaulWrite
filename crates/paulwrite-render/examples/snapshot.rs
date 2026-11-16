//! Écrit une image PNG du rendu (sans ouvrir de fenêtre) : vérification visuelle du moteur.
//!
//! Usage : `cargo run -p paulwrite-render --example snapshot -- sortie.png [échelle]`

use paulwrite_render::{Colors, Row, RowSource, SoftRenderer};

struct Demo {
    lines: Vec<(&'static str, Vec<(usize, usize)>, Vec<usize>, Option<(usize, usize)>)>,
}

impl RowSource for Demo {
    fn len(&self) -> usize {
        self.lines.len()
    }
    fn row(&self, i: usize) -> Row<'_> {
        let (text, sels, carets, underline) = &self.lines[i];
        Row {
            text,
            selections: sels,
            carets,
            underline: *underline,
        }
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// PNG RVB non compressé (blocs « stored » zlib) : suffisant pour une image de contrôle.
fn write_png(path: &str, w: u32, h: u32, bgra: &[u32]) {
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let p = bgra[y * w as usize + x];
            raw.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png).expect("écriture du PNG");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage : snapshot <sortie.png> [échelle]");
    let scale: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let (w, h) = ((760.0 * scale) as u32, (300.0 * scale) as u32);
    let mut r = SoftRenderer::new(scale, (w, h), Colors::default()).expect("rendu");
    let demo = Demo {
        lines: vec![
            ("fn main() {", vec![], vec![], None),
            ("    let message = \"héllo wörld 😀\";", vec![(4, 11)], vec![], None),
            ("\tune tabulation, puis du texte", vec![], vec![], None),
            ("    selection sur plusieurs lignes", vec![(8, 31)], vec![], None),
            ("saut de ligne sélectionné", vec![(0, 26)], vec![], None),
            ("deux curseurs ici", vec![], vec![3, 11], None),
            ("composition IME : かな", vec![], vec![21], Some((17, 19))),
            ("", vec![], vec![0], None),
            ("}", vec![], vec![], None),
        ],
    };
    r.draw(&demo, 0).expect("dessin");
    let (pw, ph) = r.size();
    write_png(&out, pw, ph, r.pixels());
    println!("écrit {out} ({pw}x{ph}, échelle {scale})");
}
