//! Rendu logiciel par régions : Direct2D (`ID2D1DCRenderTarget`) + DirectWrite dans une DIB.

use std::collections::HashMap;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_REGULAR, DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    SRCCOPY,
};
use windows_numerics::Vector2;

/// Marge gauche du texte, en pixels logiques (multipliée par l'échelle DPI).
const PAD_X: f32 = 8.0;
/// Largeur maximale affichée d'une ligne, en unités UTF-16 (au-delà : tronquée à l'affichage
/// pour qu'une ligne de plusieurs Mo ne fige pas DirectWrite).
const MAX_UNITS: usize = 8192;
/// Nombre de mises en page conservées avant vidage du cache.
const MAX_CACHED_LAYOUTS: usize = 1024;
/// Largeur du texte de police : taille en pixels logiques.
const FONT_PX: f32 = 15.0;

/// Couleurs (RGB, 0..1).
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub background: [f32; 3],
    pub text: [f32; 3],
    pub selection: [f32; 3],
    pub caret: [f32; 3],
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: [0.11, 0.12, 0.14],
            text: [0.85, 0.86, 0.88],
            selection: [0.22, 0.34, 0.55],
            caret: [0.95, 0.95, 0.95],
        }
    }
}

/// Dimensions d'un caractère à l'échelle courante, en pixels physiques.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Largeur d'un caractère (police à chasse fixe).
    pub advance: f32,
    /// Hauteur d'une rangée.
    pub line_height: i32,
    /// Décalage horizontal du texte (marge gauche).
    pub pad_x: f32,
}

impl Metrics {
    /// Nombre de rangées **entières** qui tiennent dans `height` pixels.
    #[must_use]
    pub fn rows_for(&self, height: u32) -> usize {
        (height as i32 / self.line_height.max(1)) as usize
    }
}

/// Une rangée à dessiner. Les positions sont des indices UTF-16 dans `text`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Row<'a> {
    /// Texte de la ligne, sans fin de ligne.
    pub text: &'a str,
    /// Plage sélectionnée.
    pub selection: Option<(usize, usize)>,
    /// La sélection continue au-delà de la fin de ligne (saut de ligne sélectionné).
    pub select_eol: bool,
    /// Position du curseur (absent : pas de curseur dans cette rangée, ou clignotement éteint).
    pub caret: Option<usize>,
    /// Plage soulignée (texte de composition IME).
    pub underline: Option<(usize, usize)>,
}

/// Source des rangées visibles (évite d'allouer un tableau par image).
pub trait RowSource {
    fn len(&self) -> usize;
    fn row(&self, index: usize) -> Row<'_>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Bande horizontale de pixels `[top, bottom)` redessinée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub top: i32,
    pub bottom: i32,
}

/// Signature d'une rangée dessinée : si elle est identique, les pixels le sont aussi.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sig {
    hash: u64,
    selection: Option<(usize, usize)>,
    select_eol: bool,
    caret: Option<usize>,
    underline: Option<(usize, usize)>,
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    // FNV-1a 64 bits : simple et suffisant (la clé ne sert qu'à reconnaître un texte identique).
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (bytes.len() as u64)
}

/// Texte tronqué à l'affichage (limite de [`MAX_UNITS`] unités UTF-16, sur une frontière de char).
fn displayed(text: &str) -> &str {
    if text.len() <= MAX_UNITS {
        return text;
    }
    let mut end = MAX_UNITS;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

struct CachedLayout {
    layout: IDWriteTextLayout,
    units: usize,
}

/// Moteur de rendu logiciel. Vit sur le thread UI (fabrique Direct2D mono-thread).
pub struct SoftRenderer {
    colors: Colors,
    size: (u32, u32),
    scale: f32,
    metrics: Metrics,
    // GDI
    mem_dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    // Direct2D / DirectWrite
    rt: ID2D1DCRenderTarget,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    /// 0 : texte, 1 : fond, 2 : sélection, 3 : curseur.
    brushes: Vec<ID2D1Brush>,
    layouts: HashMap<u64, CachedLayout>,
    // Régions sales
    prev: Vec<Option<Sig>>,
    cur: Vec<Option<Sig>>,
    dirty: Vec<bool>,
    bands: Vec<Band>,
    valid: bool,
}

impl std::fmt::Debug for SoftRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftRenderer")
            .field("size", &self.size)
            .field("scale", &self.scale)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

fn rgb(c: [f32; 3]) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c[0],
        g: c[1],
        b: c[2],
        a: 1.0,
    }
}

impl SoftRenderer {
    /// Crée le moteur. `scale` : facteur DPI (1.0 = 96 dpi) ; `size` : taille de la zone cliente
    /// en pixels physiques.
    ///
    /// # Errors
    /// Échec de création des objets Direct2D / DirectWrite / GDI.
    pub fn new(scale: f32, size: (u32, u32), colors: Colors) -> Result<Self> {
        let size = (size.0.max(1), size.1.max(1));
        // SAFETY: appels COM/GDI ; chaque objet créé est possédé par `Self` et libéré dans `Drop`.
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let (format, metrics) = make_format(&dwrite, scale)?;

            let mut brushes = Vec::with_capacity(4);
            for c in [
                colors.text,
                colors.background,
                colors.selection,
                colors.caret,
            ] {
                brushes.push(
                    rt.CreateSolidColorBrush(&rgb(c), None)?
                        .cast::<ID2D1Brush>()?,
                );
            }

            let mem_dc = CreateCompatibleDC(None);
            let (dib, old, bits) = make_dib(mem_dc, size.0, size.1)?;
            Ok(Self {
                colors,
                size,
                scale,
                metrics,
                mem_dc,
                dib,
                old,
                bits,
                rt,
                dwrite,
                format,
                brushes,
                layouts: HashMap::new(),
                prev: Vec::new(),
                cur: Vec::new(),
                dirty: Vec::new(),
                bands: Vec::new(),
                valid: false,
            })
        }
    }

    /// Dimensions d'un caractère à l'échelle courante.
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Taille de la DIB (pixels physiques).
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Pixels de la DIB (BGRA, ligne par ligne, du haut vers le bas). Sert aux tests.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        let len = self.size.0 as usize * self.size.1 as usize;
        // SAFETY: `bits` pointe vers la mémoire de la DIB, de `width * height` pixels de 32 bits,
        // valide tant que la DIB existe (donc tant que `self` existe) ; on ne rend qu'une lecture
        // liée à `&self`, et le rendu (qui écrit) exige `&mut self`.
        unsafe { std::slice::from_raw_parts(self.bits, len) }
    }

    /// Les bandes redessinées par le dernier [`draw`](Self::draw) et pas encore présentées.
    #[must_use]
    pub fn bands(&self) -> &[Band] {
        &self.bands
    }

    /// Force un redessin complet à la prochaine image.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Change la taille de la zone cliente.
    ///
    /// # Errors
    /// Échec de création de la DIB.
    pub fn resize(&mut self, size: (u32, u32)) -> Result<()> {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size {
            return Ok(());
        }
        // SAFETY: l'ancienne DIB est désélectionnée avant d'être détruite ; la nouvelle est
        // sélectionnée dans le même DC.
        unsafe {
            SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let (dib, old, bits) = make_dib(self.mem_dc, size.0, size.1)?;
            self.dib = dib;
            self.old = old;
            self.bits = bits;
        }
        self.size = size;
        self.valid = false;
        Ok(())
    }

    /// Change le facteur DPI (déplacement vers un autre écran, réglage Windows).
    ///
    /// # Errors
    /// Échec de création de la police.
    pub fn set_scale(&mut self, scale: f32) -> Result<()> {
        if (scale - self.scale).abs() < f32::EPSILON {
            return Ok(());
        }
        // SAFETY: création d'objets DirectWrite sur la fabrique possédée.
        let (format, metrics) = unsafe { make_format(&self.dwrite, scale)? };
        self.format = format;
        self.metrics = metrics;
        self.scale = scale;
        self.layouts.clear();
        self.valid = false;
        Ok(())
    }

    fn layout_for(&mut self, text: &str, hash: u64) -> Result<Option<(IDWriteTextLayout, usize)>> {
        let shown = displayed(text);
        if shown.is_empty() {
            return Ok(None);
        }
        if let Some(c) = self.layouts.get(&hash) {
            return Ok(Some((c.layout.clone(), c.units)));
        }
        if self.layouts.len() >= MAX_CACHED_LAYOUTS {
            self.layouts.clear();
        }
        let wide: Vec<u16> = shown.encode_utf16().collect();
        // SAFETY: création d'une mise en page DirectWrite à partir de données valides.
        let layout = unsafe {
            self.dwrite.CreateTextLayout(
                &wide,
                &self.format,
                1.0e6,
                self.metrics.line_height as f32 * 2.0,
            )?
        };
        let units = wide.len();
        self.layouts.insert(
            hash,
            CachedLayout {
                layout: layout.clone(),
                units,
            },
        );
        Ok(Some((layout, units)))
    }

    /// Redessine ce qui a changé. `scrolled_by` : de combien de rangées le contenu a défilé depuis
    /// la dernière image (positif : le texte monte, on voit des lignes plus basses). Une valeur
    /// fausse n'altère pas le résultat (les signatures sont revérifiées), elle coûte seulement du
    /// travail.
    ///
    /// Les rangées au-delà de [`Metrics::rows_for`] sont ignorées. Renvoie les bandes dessinées ;
    /// [`present`](Self::present) les copie vers la fenêtre.
    ///
    /// # Errors
    /// Échec Direct2D / DirectWrite / GDI.
    pub fn draw(&mut self, src: &dyn RowSource, scrolled_by: isize) -> Result<&[Band]> {
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let lh = self.metrics.line_height.max(1);
        let rows = src.len().min(self.metrics.rows_for(self.size.1));

        self.cur.clear();
        for r in 0..rows {
            let row = src.row(r);
            self.cur.push(Some(Sig {
                hash: hash_bytes(row.text.as_bytes()),
                selection: row.selection,
                select_eol: row.select_eol,
                caret: row.caret,
                underline: row.underline,
            }));
        }
        self.bands.clear();

        if self.valid {
            self.shift_for_scroll(scrolled_by, rows, w, lh)?;
        } else {
            self.prev.clear();
        }

        self.dirty.clear();
        for r in 0..rows {
            let clean = self.valid && self.prev.get(r).copied().flatten() == self.cur[r];
            self.dirty.push(!clean);
        }

        if self.valid {
            let mut r = 0;
            while r < rows {
                if !self.dirty[r] {
                    r += 1;
                    continue;
                }
                let start = r;
                while r < rows && self.dirty[r] {
                    r += 1;
                }
                self.bands.push(Band {
                    top: start as i32 * lh,
                    bottom: (r as i32 * lh).min(h),
                });
            }
        } else {
            // Redessin complet : tout l'écran, y compris la marge sous la dernière rangée.
            self.bands.push(Band { top: 0, bottom: h });
        }

        let bands = std::mem::take(&mut self.bands);
        let result = self.paint_bands(&bands, src, rows, w, lh);
        self.bands = bands;
        result?;

        std::mem::swap(&mut self.prev, &mut self.cur);
        self.valid = true;
        Ok(&self.bands)
    }

    /// Déplace les pixels déjà dessinés et décale les signatures (défilement).
    fn shift_for_scroll(&mut self, scrolled_by: isize, rows: usize, w: i32, lh: i32) -> Result<()> {
        let d = scrolled_by.unsigned_abs();
        if d == 0 || d >= rows.min(self.prev.len()) || self.prev.is_empty() {
            return Ok(());
        }
        let dy = d as i32 * lh;
        let region = self.prev.len() as i32 * lh;
        // SAFETY: copie de pixels à l'intérieur de la DIB sélectionnée dans `mem_dc` ; GDI gère
        // le chevauchement source/destination.
        unsafe {
            if scrolled_by > 0 {
                BitBlt(
                    self.mem_dc,
                    0,
                    0,
                    w,
                    region - dy,
                    Some(self.mem_dc),
                    0,
                    dy,
                    SRCCOPY,
                )?;
                self.prev.drain(0..d);
            } else {
                BitBlt(
                    self.mem_dc,
                    0,
                    dy,
                    w,
                    region - dy,
                    Some(self.mem_dc),
                    0,
                    0,
                    SRCCOPY,
                )?;
                self.prev.splice(0..0, std::iter::repeat(None).take(d));
            }
        }
        Ok(())
    }

    fn paint_bands(
        &mut self,
        bands: &[Band],
        src: &dyn RowSource,
        rows: usize,
        w: i32,
        lh: i32,
    ) -> Result<()> {
        let pad = self.metrics.pad_x;
        for &Band { top, bottom } in bands {
            if top >= bottom {
                continue;
            }
            // On ne lie à la cible Direct2D que la bande modifiée : `BindDC` et `EndDraw`
            // copient toute la zone liée (lier l'écran entier coûtait ~5 ms pour une ligne).
            // Les coordonnées de dessin sont relatives au haut de la bande.
            let rect = RECT {
                left: 0,
                top,
                right: w,
                bottom,
            };
            let band_h = (bottom - top) as f32;
            // SAFETY: appels Direct2D entre `BeginDraw` et `EndDraw` ; `rect` borne la DIB.
            unsafe {
                self.rt.BindDC(self.mem_dc, &rect)?;
                self.rt.BeginDraw();
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: w as f32,
                        bottom: band_h,
                    },
                    &self.brushes[1],
                );
                let (r0, r1) = ((top / lh) as usize, ((bottom + lh - 1) / lh) as usize);
                for row_idx in r0..r1.min(rows) {
                    let row = src.row(row_idx);
                    let y = (row_idx as i32 * lh - top) as f32;
                    self.paint_row(&row, pad, y, lh as f32)?;
                }
                self.rt.EndDraw(None, None)?;
            }
        }
        Ok(())
    }

    /// Dessine une rangée dans la bande en cours (appelé entre `BeginDraw` et `EndDraw`).
    unsafe fn paint_row(&mut self, row: &Row<'_>, pad: f32, y: f32, lh: f32) -> Result<()> {
        let hash = hash_bytes(row.text.as_bytes());
        let entry = self.layout_for(row.text, hash)?;
        let adv = self.metrics.advance;
        let x_at = |pos: usize| -> f32 {
            match &entry {
                None => pad,
                Some((layout, units)) => {
                    let (mut x, mut yy) = (0.0_f32, 0.0_f32);
                    let mut m = DWRITE_HIT_TEST_METRICS::default();
                    let _ = layout.HitTestTextPosition(
                        pos.min(*units) as u32,
                        false,
                        &mut x,
                        &mut yy,
                        &mut m,
                    );
                    pad + x
                }
            }
        };
        if let Some((a, b)) = row.selection {
            let x0 = x_at(a);
            let mut x1 = x_at(b);
            if row.select_eol {
                x1 += adv;
            }
            if x1 > x0 {
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: x0,
                        top: y,
                        right: x1,
                        bottom: y + lh,
                    },
                    &self.brushes[2],
                );
            }
        }
        if let Some((layout, _)) = &entry {
            self.rt.DrawTextLayout(
                Vector2 { X: pad, Y: y },
                layout,
                &self.brushes[0],
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        if let Some((a, b)) = row.underline {
            let (x0, x1) = (x_at(a), x_at(b));
            self.rt.FillRectangle(
                &D2D_RECT_F {
                    left: x0,
                    top: y + lh - 2.0,
                    right: x1.max(x0 + 1.0),
                    bottom: y + lh - 1.0,
                },
                &self.brushes[0],
            );
        }
        if let Some(c) = row.caret {
            let x = x_at(c);
            self.rt.FillRectangle(
                &D2D_RECT_F {
                    left: x,
                    top: y,
                    right: x + (2.0 * self.scale).round().max(1.0),
                    bottom: y + lh,
                },
                &self.brushes[3],
            );
        }
        Ok(())
    }

    /// Copie vers la fenêtre les bandes du dernier [`draw`](Self::draw).
    ///
    /// # Errors
    /// Échec GDI.
    pub fn present(&mut self, hwnd: isize) -> Result<()> {
        let bands = std::mem::take(&mut self.bands);
        let r = self.blit(hwnd, &bands);
        self.bands = bands;
        self.bands.clear();
        r
    }

    /// Copie toute la DIB vers la fenêtre (fenêtre redécouverte, `WM_PAINT`).
    ///
    /// # Errors
    /// Échec GDI.
    pub fn present_all(&mut self, hwnd: isize) -> Result<()> {
        let all = [Band {
            top: 0,
            bottom: self.size.1 as i32,
        }];
        self.bands.clear();
        self.blit(hwnd, &all)
    }

    fn blit(&self, hwnd: isize, bands: &[Band]) -> Result<()> {
        if bands.is_empty() {
            return Ok(());
        }
        let w = self.size.0 as i32;
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: le DC de la fenêtre est obtenu puis libéré dans cette fonction ; `mem_dc`
        // contient la DIB.
        unsafe {
            let dc = GetDC(Some(hwnd));
            let mut result = Ok(());
            for b in bands {
                if let Err(e) = BitBlt(
                    dc,
                    0,
                    b.top,
                    w,
                    b.bottom - b.top,
                    Some(self.mem_dc),
                    0,
                    b.top,
                    SRCCOPY,
                ) {
                    result = Err(e);
                    break;
                }
            }
            ReleaseDC(Some(hwnd), dc);
            result
        }
    }

    /// Couleurs utilisées.
    #[must_use]
    pub fn colors(&self) -> Colors {
        self.colors
    }
}

/// Crée la police à chasse fixe à l'échelle donnée et mesure un caractère.
unsafe fn make_format(dwrite: &IDWriteFactory, scale: f32) -> Result<(IDWriteTextFormat, Metrics)> {
    let format = dwrite.CreateTextFormat(
        w!("Consolas"),
        None,
        DWRITE_FONT_WEIGHT_REGULAR,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        FONT_PX * scale,
        w!("en-us"),
    )?;
    format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let probe: Vec<u16> = "M".encode_utf16().collect();
    let layout = dwrite.CreateTextLayout(&probe, &format, 1000.0, 1000.0)?;
    let mut m = DWRITE_TEXT_METRICS::default();
    layout.GetMetrics(&mut m)?;
    let advance = m.width;
    format.SetIncrementalTabStop(advance * 4.0)?;
    Ok((
        format,
        Metrics {
            advance,
            line_height: m.height.ceil() as i32,
            pad_x: (PAD_X * scale).round(),
        },
    ))
}

/// Crée une DIB 32 bits « top-down » et la sélectionne dans `mem_dc`.
unsafe fn make_dib(mem_dc: HDC, w: u32, h: u32) -> Result<(HBITMAP, HGDIOBJ, *mut u32)> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let old = SelectObject(mem_dc, dib.into());
    Ok((dib, old, bits.cast()))
}

impl Drop for SoftRenderer {
    fn drop(&mut self) {
        // SAFETY: libère les objets GDI créés dans `new` / `resize`, dans l'ordre inverse.
        unsafe {
            SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rows {
        texts: Vec<String>,
        caret: Option<(usize, usize)>,
        selection: Option<(usize, usize, usize)>,
    }

    impl RowSource for Rows {
        fn len(&self) -> usize {
            self.texts.len()
        }
        fn row(&self, i: usize) -> Row<'_> {
            Row {
                text: &self.texts[i],
                selection: self
                    .selection
                    .and_then(|(r, a, b)| (r == i).then_some((a, b))),
                select_eol: false,
                caret: self.caret.and_then(|(r, c)| (r == i).then_some(c)),
                underline: None,
            }
        }
    }

    fn renderer() -> SoftRenderer {
        SoftRenderer::new(1.0, (640, 300), Colors::default()).expect("renderer")
    }

    fn lines(first: usize, n: usize) -> Vec<String> {
        (first..first + n)
            .map(|i| {
                format!(
                    "{}ligne {i} fn main() {{ let x = {}; }}",
                    " ".repeat(i % 3 * 4),
                    i * 7
                )
            })
            .collect()
    }

    /// Le rendu par régions doit produire exactement les mêmes pixels qu'un redessin complet.
    #[test]
    fn regions_sales_identiques_au_redessin_complet() {
        let mut inc = renderer();
        let mut full = renderer();
        let rows_n = inc.metrics().rows_for(300);
        assert!(rows_n > 5);

        let mut rng = 0x1234_5678_9abc_def0_u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        let mut first = 100usize;
        let mut state = Rows {
            texts: lines(first, rows_n),
            caret: Some((2, 3)),
            selection: None,
        };
        inc.draw(&state, 0).unwrap();

        for step in 0..120 {
            let mut scrolled = 0isize;
            match next() % 5 {
                0 => {
                    // édition d'une ligne
                    let r = (next() % rows_n as u64) as usize;
                    state.texts[r].push_str("xyz");
                }
                1 => {
                    // défilement vers le bas
                    let d = (next() % 4 + 1) as usize;
                    first += d;
                    state.texts = lines(first, rows_n);
                    scrolled = d as isize;
                }
                2 => {
                    // défilement vers le haut
                    let d = (next() % 4 + 1) as usize;
                    first -= d;
                    state.texts = lines(first, rows_n);
                    scrolled = -(d as isize);
                }
                3 => {
                    state.caret = Some(((next() % rows_n as u64) as usize, (next() % 10) as usize));
                }
                _ => {
                    let r = (next() % rows_n as u64) as usize;
                    state.selection = Some((r, (next() % 5) as usize, 5 + (next() % 10) as usize));
                }
            }
            // une valeur de défilement fausse ne doit rien changer au résultat
            let hint = if step % 7 == 0 {
                scrolled + 1
            } else {
                scrolled
            };
            inc.draw(&state, hint).unwrap();
            full.invalidate();
            full.draw(&state, 0).unwrap();
            assert!(
                inc.pixels() == full.pixels(),
                "pixels différents à l'étape {step}"
            );
        }
    }

    #[test]
    fn une_frappe_ne_redessine_qu_une_bande() {
        let mut r = renderer();
        let n = r.metrics().rows_for(300);
        let mut s = Rows {
            texts: lines(0, n),
            caret: Some((3, 0)),
            selection: None,
        };
        r.draw(&s, 0).unwrap();
        s.texts[3].push('a');
        let lh = r.metrics().line_height;
        let bands = r.draw(&s, 0).unwrap().to_vec();
        assert_eq!(
            bands,
            vec![Band {
                top: 3 * lh,
                bottom: 4 * lh
            }]
        );
        // aucune modification : rien à redessiner
        assert!(r.draw(&s, 0).unwrap().is_empty());
    }

    #[test]
    fn le_texte_dessine_des_pixels_et_le_fond_est_uni() {
        let mut r = renderer();
        let s = Rows {
            texts: vec!["Hello".into(), String::new()],
            caret: None,
            selection: None,
        };
        r.draw(&s, 0).unwrap();
        let (w, _) = r.size();
        let lh = r.metrics().line_height as usize;
        let px = r.pixels();
        let bg = px[px.len() - 1];
        assert!(
            px[..lh * w as usize].iter().any(|&p| p != bg),
            "texte absent"
        );
        // la deuxième rangée (vide) est uniformément du fond
        assert!(px[lh * w as usize..2 * lh * w as usize]
            .iter()
            .all(|&p| p == bg));
    }

    #[test]
    fn redimensionnement_et_changement_d_echelle() {
        let mut r = renderer();
        let m1 = r.metrics();
        r.set_scale(2.0).unwrap();
        assert!(r.metrics().line_height > m1.line_height);
        r.resize((800, 400)).unwrap();
        assert_eq!(r.size(), (800, 400));
        let s = Rows {
            texts: lines(0, 3),
            caret: Some((0, 0)),
            selection: None,
        };
        r.draw(&s, 0).unwrap();
    }

    #[test]
    fn ligne_geante_tronquee_sans_planter() {
        let mut r = renderer();
        let s = Rows {
            texts: vec!["é".repeat(1_000_000)],
            caret: Some((0, 5)),
            selection: Some((0, 0, 20)),
        };
        r.draw(&s, 0).unwrap();
    }
}
