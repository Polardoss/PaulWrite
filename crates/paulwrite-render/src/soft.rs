//! Rendu logiciel par régions : Direct2D (`ID2D1DCRenderTarget`) + DirectWrite dans une DIB.

use std::collections::HashMap;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_REGULAR, DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
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

/// Indices des pinceaux de la barre d'onglets (après texte, fond, sélection, curseur et les
/// catégories de coloration).
const BR_TABBAR: usize = 3 + KIND_SLOTS;
const BR_TAB_DIM: usize = BR_TABBAR + 1;
const BR_TAB_ACCENT: usize = BR_TABBAR + 2;
/// Fond du panneau flottant (palette de commandes, Goto Anything).
const BR_OVERLAY_BG: usize = BR_TABBAR + 3;
/// Soulignements des diagnostics : erreur, avertissement, information (3 pinceaux consécutifs).
const BR_SQUIGGLE: usize = BR_TABBAR + 4;
/// Index de la première rangée de l'éditeur recouverte par le panneau flottant.
const OVERLAY_FIRST_ROW: usize = 1;
/// Catégorie de coloration des caractères qui correspondent à la saisie dans le panneau.
const OVERLAY_MATCH_KIND: u8 = 4;

/// Couleurs (RGB, 0..1).
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub background: [f32; 3],
    pub text: [f32; 3],
    pub selection: [f32; 3],
    pub caret: [f32; 3],
    /// Couleurs de coloration syntaxique, indexées par catégorie (`Row::spans`). L'indice 0 est
    /// ignoré : le texte ordinaire utilise `text`.
    pub kinds: [[f32; 3]; KIND_SLOTS],
}

/// Nombre de catégories de coloration gérées (indice 0 compris).
pub const KIND_SLOTS: usize = 16;

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: [0.11, 0.12, 0.14],
            text: [0.85, 0.86, 0.88],
            selection: [0.22, 0.34, 0.55],
            caret: [0.95, 0.95, 0.95],
            kinds: DEFAULT_KINDS,
        }
    }
}

/// Palette par défaut (thème sombre). Ordre = catégories du crate `paulwrite-syntax` :
/// 1 commentaire, 2 chaîne, 3 nombre, 4 mot-clé, 5 fonction, 6 type, 7 constante, 8 propriété,
/// 9 opérateur, 10 ponctuation, 11 attribut/macro, 12 balise, 13 échappement, 14 prédéfini.
const DEFAULT_KINDS: [[f32; 3]; KIND_SLOTS] = [
    [0.85, 0.86, 0.88],
    [0.42, 0.52, 0.40],
    [0.81, 0.60, 0.47],
    [0.71, 0.81, 0.66],
    [0.34, 0.61, 0.84],
    [0.86, 0.86, 0.67],
    [0.31, 0.79, 0.69],
    [0.31, 0.76, 1.00],
    [0.61, 0.86, 1.00],
    [0.85, 0.86, 0.88],
    [0.62, 0.64, 0.68],
    [0.77, 0.52, 0.75],
    [0.34, 0.61, 0.84],
    [0.84, 0.74, 0.40],
    [0.34, 0.61, 0.84],
    [0.85, 0.86, 0.88],
];

/// Dimensions d'un caractère à l'échelle courante, en pixels physiques.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Largeur d'un caractère (police à chasse fixe).
    pub advance: f32,
    /// Hauteur d'une rangée.
    pub line_height: i32,
    /// Décalage horizontal du texte (marge gauche).
    pub pad_x: f32,
    /// Hauteur de la barre d'onglets lorsqu'elle est affichée.
    pub tab_bar: i32,
    /// Ordonnée de la première rangée : 0 sans onglets, `tab_bar` avec.
    pub top: i32,
}

impl Metrics {
    /// Nombre de rangées **entières** qui tiennent dans `height` pixels (sous la barre d'onglets).
    #[must_use]
    pub fn rows_for(&self, height: u32) -> usize {
        ((height as i32 - self.top).max(0) / self.line_height.max(1)) as usize
    }
}

/// Un onglet à afficher dans la barre.
#[derive(Clone, Copy, Debug)]
pub struct TabInfo<'a> {
    pub label: &'a str,
    /// Contenu modifié : une pastille remplace la croix de fermeture.
    pub modified: bool,
    pub active: bool,
}

/// Nature d'une rangée du panneau flottant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayRowKind {
    /// Résultat ordinaire.
    #[default]
    Item,
    /// Résultat sélectionné (surligné).
    Selected,
    /// Ligne de saisie (avec curseur).
    Input,
    /// Message discret (aucun résultat, état de l'indexation…).
    Note,
}

/// Une rangée du panneau flottant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverlayRow {
    pub kind: OverlayRowKind,
    /// Texte principal. L'appelant le tronque à [`SoftRenderer::overlay_columns`] caractères.
    pub text: String,
    /// Texte secondaire, aligné à droite (raccourci, dossier…).
    pub hint: String,
    /// Position du curseur dans `text` (UTF-16), pour la ligne de saisie.
    pub caret: Option<usize>,
    /// Plages de `text` (UTF-16) qui correspondent à la saisie, mises en évidence.
    pub highlights: Vec<(usize, usize)>,
}

/// Panneau flottant dessiné au-dessus du texte : première rangée = saisie, puis résultats.
/// Il occupe des rangées entières de l'éditeur (à partir de la deuxième) et est centré.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub rows: Vec<OverlayRow>,
}

fn overlay_row_hash(r: &OverlayRow) -> u64 {
    let mut h = hash_bytes(r.text.as_bytes()) ^ hash_bytes(r.hint.as_bytes()).rotate_left(17);
    let mut mix = |v: usize| {
        h ^= v as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    mix(r.kind as usize);
    mix(r.caret.unwrap_or(usize::MAX));
    for &(a, b) in &r.highlights {
        mix(a);
        mix(b);
    }
    h
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TabEntry {
    label: String,
    modified: bool,
    active: bool,
}

/// Une rangée à dessiner. Les positions sont des indices UTF-16 dans `text`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Row<'a> {
    /// Texte de la ligne, sans fin de ligne.
    pub text: &'a str,
    /// Plages sélectionnées. Une fin supérieure à la longueur UTF-16 du texte signifie que le
    /// saut de ligne est sélectionné (la surbrillance déborde d'un caractère).
    pub selections: &'a [(usize, usize)],
    /// Positions des curseurs (vide : aucun curseur dans cette rangée, ou clignotement éteint).
    pub carets: &'a [usize],
    /// Plage soulignée (texte de composition IME).
    pub underline: Option<(usize, usize)>,
    /// Plages colorées `(début, fin, catégorie)` en UTF-16, triées et sans chevauchement.
    pub spans: &'a [(usize, usize, u8)],
    /// Soulignements ondulés `(début, fin, gravité)` en UTF-16 : diagnostics d'un serveur de
    /// langage. Gravité : 0 erreur, 1 avertissement, 2 information.
    pub squiggles: &'a [(usize, usize, u8)],
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
    /// Empreinte du texte.
    hash: u64,
    /// Empreinte des décorations (sélections, curseurs, soulignement).
    deco: u64,
}

/// Empreinte des décorations d'une rangée.
fn deco_hash(row: &Row<'_>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    let mut mix = |v: usize| {
        h ^= v as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    for &(a, b) in row.selections {
        mix(a);
        mix(b);
    }
    mix(usize::MAX);
    for &c in row.carets {
        mix(c);
    }
    mix(usize::MAX - 1);
    if let Some((a, b)) = row.underline {
        mix(a);
        mix(b);
    }
    mix(usize::MAX - 2);
    for &(a, b, k) in row.spans {
        mix(a);
        mix(b);
        mix(usize::from(k));
    }
    mix(usize::MAX - 3);
    for &(a, b, k) in row.squiggles {
        mix(a);
        mix(b);
        mix(usize::from(k));
    }
    h
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
    /// La mise en page porte des couleurs par plage (à réinitialiser avant de la réutiliser sans).
    colored: bool,
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
    /// Zone des rangées dont les pixels ont été **déplacés** par un défilement : elle a changé
    /// dans la DIB sans être redessinée, donc [`present`](Self::present) doit la recopier vers la
    /// fenêtre en plus des bandes dessinées (sinon le milieu de l'écran garde l'ancien contenu).
    shift_region: Option<Band>,
    valid: bool,
    // Barre d'onglets
    tabs: Vec<TabEntry>,
    tabs_clean: bool,
    // Panneau flottant
    overlay: Option<Overlay>,
    overlay_hashes: Vec<u64>,
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

            // 0 texte, 1 fond, 2 sélection, 3 curseur, puis 3 + catégorie (1..KIND_SLOTS).
            let mut brushes = Vec::with_capacity(6 + KIND_SLOTS);
            for c in [
                colors.text,
                colors.background,
                colors.selection,
                colors.caret,
            ]
            .into_iter()
            .chain(colors.kinds.iter().skip(1).copied())
            {
                brushes.push(
                    rt.CreateSolidColorBrush(&rgb(c), None)?
                        .cast::<ID2D1Brush>()?,
                );
            }

            // Barre d'onglets : fond, texte discret, accent.
            let dim = |c: [f32; 3], k: f32| [c[0] * k, c[1] * k, c[2] * k];
            let lift = |c: [f32; 3]| [c[0] + 0.07, c[1] + 0.07, c[2] + 0.07];
            for c in [
                dim(colors.background, 0.72),
                dim(colors.text, 0.58),
                colors.kinds[4],
                lift(colors.background),
                [0.93, 0.33, 0.33],
                [0.90, 0.75, 0.30],
                [0.35, 0.65, 0.95],
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
                shift_region: None,
                valid: false,
                tabs: Vec::new(),
                tabs_clean: false,
                overlay: None,
                overlay_hashes: Vec::new(),
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
        // La hauteur de la barre d'onglets dépend de l'échelle.
        self.metrics.top = if self.tabs.is_empty() {
            0
        } else {
            self.metrics.tab_bar
        };
        self.layouts.clear();
        self.tabs_clean = false;
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
                colored: false,
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
        let top = self.metrics.top;
        let rows = src.len().min(self.metrics.rows_for(self.size.1));

        self.cur.clear();
        for r in 0..rows {
            let row = src.row(r);
            let mut deco = deco_hash(&row);
            // Les rangées recouvertes par le panneau dépendent aussi de son contenu : elles sont
            // redessinées (texte puis panneau par-dessus) quand l'un ou l'autre change.
            if r >= OVERLAY_FIRST_ROW {
                if let Some(h) = self.overlay_hashes.get(r - OVERLAY_FIRST_ROW) {
                    deco ^= *h | 1;
                }
            }
            self.cur.push(Some(Sig {
                hash: hash_bytes(row.text.as_bytes()),
                deco,
            }));
        }
        self.bands.clear();

        if self.valid {
            self.shift_for_scroll(scrolled_by, rows, w, lh, top)?;
        } else {
            self.prev.clear();
        }

        self.dirty.clear();
        for r in 0..rows {
            let clean = self.valid && self.prev.get(r).copied().flatten() == self.cur[r];
            self.dirty.push(!clean);
        }

        if self.valid {
            if !self.tabs_clean && top > 0 {
                self.bands.push(Band {
                    top: 0,
                    bottom: top,
                });
            }
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
                    top: top + start as i32 * lh,
                    bottom: (top + r as i32 * lh).min(h),
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
        self.tabs_clean = true;

        std::mem::swap(&mut self.prev, &mut self.cur);
        self.valid = true;
        Ok(&self.bands)
    }

    /// Déplace les pixels déjà dessinés et décale les signatures (défilement).
    fn shift_for_scroll(
        &mut self,
        scrolled_by: isize,
        rows: usize,
        w: i32,
        lh: i32,
        top: i32,
    ) -> Result<()> {
        let d = scrolled_by.unsigned_abs();
        if d == 0 || d >= rows.min(self.prev.len()) || self.prev.is_empty() {
            return Ok(());
        }
        let dy = d as i32 * lh;
        let region = self.prev.len() as i32 * lh;
        self.shift_region = Some(Band {
            top,
            bottom: (top + region).min(self.size.1 as i32),
        });
        // SAFETY: copie de pixels à l'intérieur de la DIB sélectionnée dans `mem_dc` ; GDI gère
        // le chevauchement source/destination.
        unsafe {
            if scrolled_by > 0 {
                BitBlt(
                    self.mem_dc,
                    0,
                    top,
                    w,
                    region - dy,
                    Some(self.mem_dc),
                    0,
                    top + dy,
                    SRCCOPY,
                )?;
                self.prev.drain(0..d);
            } else {
                BitBlt(
                    self.mem_dc,
                    0,
                    top + dy,
                    w,
                    region - dy,
                    Some(self.mem_dc),
                    0,
                    top,
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
        let first_row_y = self.metrics.top;
        for &Band {
            top: bt,
            bottom: bb,
        } in bands
        {
            if bt >= bb {
                continue;
            }
            // On ne lie à la cible Direct2D que la bande modifiée : `BindDC` et `EndDraw`
            // copient toute la zone liée (lier l'écran entier coûtait ~5 ms pour une ligne).
            // Les coordonnées de dessin sont relatives au haut de la bande.
            let rect = RECT {
                left: 0,
                top: bt,
                right: w,
                bottom: bb,
            };
            let band_h = (bb - bt) as f32;
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
                if bt < first_row_y {
                    self.paint_tabs(w, -(bt as f32))?;
                }
                if bb > first_row_y {
                    let r0 = ((bt - first_row_y).max(0) / lh) as usize;
                    let r1 = ((bb - first_row_y + lh - 1) / lh) as usize;
                    for row_idx in r0..r1.min(rows) {
                        let row = src.row(row_idx);
                        let y = (first_row_y + row_idx as i32 * lh - bt) as f32;
                        self.paint_row(&row, pad, y, lh as f32)?;
                        if row_idx >= OVERLAY_FIRST_ROW {
                            let i = row_idx - OVERLAY_FIRST_ROW;
                            if i < self.overlay_hashes.len() {
                                self.paint_overlay_row(i, y, lh as f32)?;
                            }
                        }
                    }
                }
                self.rt.EndDraw(None, None)?;
            }
        }
        Ok(())
    }

    // --- Panneau flottant --------------------------------------------------------------------

    fn overlay_pad(&self) -> f32 {
        (10.0 * self.scale).round()
    }

    /// Abscisses `(gauche, droite)` du panneau : centré, ~80 % de la largeur (20 à 90 colonnes).
    fn overlay_box(&self) -> (i32, i32) {
        let w = self.size.0 as f32;
        let adv = self.metrics.advance.max(1.0);
        let pad = self.overlay_pad();
        let cols = (((w * 0.8 - 2.0 * pad) / adv).floor() as usize).clamp(20, 90);
        let box_w = (cols as f32 * adv + 2.0 * pad).min(w);
        let x0 = ((w - box_w) / 2.0).floor();
        (x0 as i32, (x0 + box_w) as i32)
    }

    /// Nombre de caractères qui tiennent sur une rangée du panneau.
    #[must_use]
    pub fn overlay_columns(&self) -> usize {
        let (x0, x1) = self.overlay_box();
        let inner = (x1 - x0) as f32 - 2.0 * self.overlay_pad();
        (inner / self.metrics.advance.max(1.0)).floor().max(1.0) as usize
    }

    /// Affiche, remplace ou retire (`None`) le panneau flottant. Seules les rangées dont le
    /// contenu change sont redessinées.
    pub fn set_overlay(&mut self, overlay: Option<&Overlay>) {
        match overlay {
            Some(o) if self.overlay.as_ref() == Some(o) => {}
            Some(o) => {
                self.overlay_hashes = o.rows.iter().map(overlay_row_hash).collect();
                self.overlay = Some(o.clone());
            }
            None => {
                self.overlay = None;
                self.overlay_hashes.clear();
            }
        }
    }

    /// Rangée du panneau située sous le point `(x, y)` (pixels de la zone cliente), ou `None`
    /// si le point est hors du panneau (ou s'il n'y en a pas).
    #[must_use]
    pub fn overlay_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let o = self.overlay.as_ref()?;
        let (x0, x1) = self.overlay_box();
        if x < x0 as f32 || x >= x1 as f32 || y < self.metrics.top as f32 {
            return None;
        }
        let row = ((y as i32 - self.metrics.top) / self.metrics.line_height.max(1)) as usize;
        let i = row.checked_sub(OVERLAY_FIRST_ROW)?;
        (i < o.rows.len()).then_some(i)
    }

    /// Dessine la rangée `i` du panneau à l'ordonnée `y` de la bande en cours.
    unsafe fn paint_overlay_row(&mut self, i: usize, y: f32, lh: f32) -> Result<()> {
        let Some((row, last)) = self
            .overlay
            .as_ref()
            .map(|o| (o.rows[i].clone(), o.rows.len() - 1))
        else {
            return Ok(());
        };
        let (x0, x1) = self.overlay_box();
        let (x0, x1) = (x0 as f32, x1 as f32);
        let pad = self.overlay_pad();
        let adv = self.metrics.advance;
        let fill = if row.kind == OverlayRowKind::Selected {
            2
        } else {
            BR_OVERLAY_BG
        };
        let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F {
            left: l,
            top: t,
            right: r,
            bottom: b,
        };
        self.rt
            .FillRectangle(&rect(x0, y, x1, y + lh), &self.brushes[fill]);
        // Bordure : traits latéraux sur chaque rangée, haut et bas sur la première et la dernière.
        let dim = &self.brushes[BR_TAB_DIM];
        self.rt.FillRectangle(&rect(x0, y, x0 + 1.0, y + lh), dim);
        self.rt.FillRectangle(&rect(x1 - 1.0, y, x1, y + lh), dim);
        if i == 0 {
            self.rt.FillRectangle(&rect(x0, y, x1, y + 1.0), dim);
        }
        if i == last {
            self.rt
                .FillRectangle(&rect(x0, y + lh - 1.0, x1, y + lh), dim);
        }

        self.rt.PushAxisAlignedClip(
            &rect(x0 + 1.0, y, x1 - 1.0, y + lh),
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        let spans: Vec<(usize, usize, u8)> = row
            .highlights
            .iter()
            .map(|&(a, b)| (a, b, OVERLAY_MATCH_KIND))
            .collect();
        let hash = hash_bytes(row.text.as_bytes());
        let entry = self.layout_for(&row.text, hash)?;
        if let Some((layout, units)) = &entry {
            self.apply_colors(hash, layout, *units, &spans)?;
            let brush = if row.kind == OverlayRowKind::Note {
                BR_TAB_DIM
            } else {
                0
            };
            self.rt.DrawTextLayout(
                Vector2 { X: x0 + pad, Y: y },
                layout,
                &self.brushes[brush],
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        if !row.hint.is_empty() {
            let hhash = hash_bytes(row.hint.as_bytes());
            let chars = row.hint.chars().count() as f32;
            if let Some((layout, units)) = self.layout_for(&row.hint, hhash)? {
                self.apply_colors(hhash, &layout, units, &[])?;
                self.rt.DrawTextLayout(
                    Vector2 {
                        X: x1 - pad - chars * adv,
                        Y: y,
                    },
                    &layout,
                    &self.brushes[BR_TAB_DIM],
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
        if let Some(c) = row.caret {
            let x = match &entry {
                None => x0 + pad,
                Some((layout, units)) => {
                    let (mut cx, mut cy) = (0.0_f32, 0.0_f32);
                    let mut m = DWRITE_HIT_TEST_METRICS::default();
                    let _ = layout.HitTestTextPosition(
                        c.min(*units) as u32,
                        false,
                        &mut cx,
                        &mut cy,
                        &mut m,
                    );
                    x0 + pad + cx
                }
            };
            self.rt.FillRectangle(
                &rect(
                    x,
                    y + 2.0,
                    x + (2.0 * self.scale).round().max(1.0),
                    y + lh - 2.0,
                ),
                &self.brushes[3],
            );
        }
        self.rt.PopAxisAlignedClip();
        Ok(())
    }

    // --- Barre d'onglets ---------------------------------------------------------------------

    /// Définit les onglets affichés (liste vide : pas de barre). Un changement de contenu ne
    /// redessine que la barre ; l'apparition ou la disparition de la barre décale les rangées
    /// et force un redessin complet.
    pub fn set_tabs(&mut self, tabs: &[TabInfo<'_>]) {
        let entries: Vec<TabEntry> = tabs
            .iter()
            .map(|t| TabEntry {
                label: t.label.to_owned(),
                modified: t.modified,
                active: t.active,
            })
            .collect();
        if entries != self.tabs {
            self.tabs = entries;
            self.tabs_clean = false;
        }
        let top = if self.tabs.is_empty() {
            0
        } else {
            self.metrics.tab_bar
        };
        if top != self.metrics.top {
            self.metrics.top = top;
            self.valid = false;
        }
    }

    /// Géométrie des onglets : `(x0, x1, nombre de caractères du libellé affiché)`.
    fn tab_geometry(&self) -> Vec<(i32, i32, usize)> {
        let n = self.tabs.len().max(1) as i32;
        let adv = self.metrics.advance;
        let pad = (12.0 * self.scale).round();
        let gap = (8.0 * self.scale).round();
        let w = self.size.0 as f32;
        let share = w / n as f32;
        let max_chars = (((share - 2.0 * pad - gap - adv) / adv).floor() as usize).clamp(4, 28);
        let mut x = 0.0_f32;
        self.tabs
            .iter()
            .map(|t| {
                let chars = t.label.chars().count().min(max_chars);
                let width = 2.0 * pad + chars as f32 * adv + gap + adv;
                let x0 = x;
                x += width;
                (x0 as i32, x as i32, chars)
            })
            .collect()
    }

    /// Onglet situé sous le point `(x, y)` (pixels de la zone cliente) et, si le point est sur la
    /// croix/pastille de fermeture, `true`.
    #[must_use]
    pub fn tab_at(&self, x: f32, y: f32) -> Option<(usize, bool)> {
        if self.tabs.is_empty() || y < 0.0 || y >= self.metrics.tab_bar as f32 {
            return None;
        }
        let pad = (12.0 * self.scale).round();
        let slot = self.metrics.advance + pad;
        for (i, (x0, x1, _)) in self.tab_geometry().into_iter().enumerate() {
            if x >= x0 as f32 && x < x1 as f32 {
                return Some((i, x >= x1 as f32 - slot - 2.0));
            }
        }
        None
    }

    /// Dessine la barre d'onglets ; `y0` est l'ordonnée du haut de la barre dans la bande en cours.
    unsafe fn paint_tabs(&mut self, w: i32, y0: f32) -> Result<()> {
        let bar_h = self.metrics.tab_bar as f32;
        self.rt.FillRectangle(
            &D2D_RECT_F {
                left: 0.0,
                top: y0,
                right: w as f32,
                bottom: y0 + bar_h,
            },
            &self.brushes[BR_TABBAR],
        );
        let geometry = self.tab_geometry();
        let pad = (12.0 * self.scale).round();
        let adv = self.metrics.advance;
        let lh = self.metrics.line_height as f32;
        let ty = y0 + ((bar_h - lh) / 2.0).floor();
        let accent_h = (2.0 * self.scale).round().max(1.0);
        for (i, (x0, x1, chars)) in geometry.into_iter().enumerate() {
            let tab = self.tabs[i].clone();
            let (xf0, xf1) = (x0 as f32, x1 as f32);
            if tab.active {
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: xf0,
                        top: y0,
                        right: xf1,
                        bottom: y0 + bar_h,
                    },
                    &self.brushes[1],
                );
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: xf0,
                        top: y0,
                        right: xf1,
                        bottom: y0 + accent_h,
                    },
                    &self.brushes[BR_TAB_ACCENT],
                );
            }
            // Libellé tronqué avec une ellipse s'il dépasse la place disponible.
            let total = tab.label.chars().count();
            let shown: String = if total > chars {
                tab.label
                    .chars()
                    .take(chars.saturating_sub(1))
                    .chain(std::iter::once('\u{2026}'))
                    .collect()
            } else {
                tab.label.clone()
            };
            let text_brush = if tab.active { 0 } else { BR_TAB_DIM };
            let hash = hash_bytes(shown.as_bytes());
            if let Some((layout, units)) = self.layout_for(&shown, hash)? {
                self.apply_colors(hash, &layout, units, &[])?;
                self.rt.DrawTextLayout(
                    Vector2 {
                        X: xf0 + pad,
                        Y: ty,
                    },
                    &layout,
                    &self.brushes[text_brush],
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            // Pastille (modifié) ou croix (fermer), dans la dernière colonne de l'onglet.
            let mark = if tab.modified { "\u{25CF}" } else { "\u{00D7}" };
            let mhash = hash_bytes(mark.as_bytes());
            if let Some((layout, units)) = self.layout_for(mark, mhash)? {
                self.apply_colors(mhash, &layout, units, &[])?;
                let brush = if tab.modified {
                    BR_TAB_ACCENT
                } else {
                    BR_TAB_DIM
                };
                self.rt.DrawTextLayout(
                    Vector2 {
                        X: xf1 - pad - adv,
                        Y: ty,
                    },
                    &layout,
                    &self.brushes[brush],
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
        Ok(())
    }

    /// Applique (ou retire) les couleurs par plage d'une mise en page mise en cache : la même
    /// mise en page sert à toutes les rangées de même texte, donc ses couleurs sont refixées à
    /// chaque dessin si elle en a ou en a eu.
    unsafe fn apply_colors(
        &mut self,
        hash: u64,
        layout: &IDWriteTextLayout,
        units: usize,
        spans: &[(usize, usize, u8)],
    ) -> Result<()> {
        let was_colored = self.layouts.get(&hash).is_some_and(|c| c.colored);
        if spans.is_empty() && !was_colored {
            return Ok(());
        }
        let base: windows::core::IUnknown = self.brushes[0].cast()?;
        layout.SetDrawingEffect(
            &base,
            DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: units as u32,
            },
        )?;
        for &(a, b, k) in spans {
            let (a, b) = (a.min(units), b.min(units));
            if a >= b || k == 0 || usize::from(k) >= KIND_SLOTS {
                continue;
            }
            let brush: windows::core::IUnknown = self.brushes[3 + usize::from(k)].cast()?;
            layout.SetDrawingEffect(
                &brush,
                DWRITE_TEXT_RANGE {
                    startPosition: a as u32,
                    length: (b - a) as u32,
                },
            )?;
        }
        if let Some(c) = self.layouts.get_mut(&hash) {
            c.colored = !spans.is_empty();
        }
        Ok(())
    }

    /// Dessine une rangée dans la bande en cours (appelé entre `BeginDraw` et `EndDraw`).
    unsafe fn paint_row(&mut self, row: &Row<'_>, pad: f32, y: f32, lh: f32) -> Result<()> {
        let hash = hash_bytes(row.text.as_bytes());
        let entry = self.layout_for(row.text, hash)?;
        let adv = self.metrics.advance;
        let units_len = entry.as_ref().map_or(0, |(_, u)| *u);
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
        for &(a, b) in row.selections {
            let x0 = x_at(a);
            let mut x1 = x_at(b);
            if b > units_len {
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
        if let Some((layout, units)) = &entry {
            self.apply_colors(hash, layout, *units, row.spans)?;
            self.rt.DrawTextLayout(
                Vector2 { X: pad, Y: y },
                layout,
                &self.brushes[0],
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        for &(a, b, severity) in row.squiggles {
            let x0 = x_at(a);
            let x1 = if b > a { x_at(b) } else { x0 + adv };
            // Ligne ondulée : de petits segments alternativement en haut et en bas.
            let brush = BR_SQUIGGLE + usize::from(severity.min(2));
            let step = (2.0 * self.scale).round().max(2.0);
            let thick = self.scale.round().max(1.0);
            let base = y + lh - 2.0 * thick - 1.0;
            let mut x = x0;
            let mut up = false;
            let mut n = 0;
            while x < x1 && n < 600 {
                let off = if up { 0.0 } else { thick };
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: x,
                        top: base + off,
                        right: (x + step).min(x1),
                        bottom: base + off + thick,
                    },
                    &self.brushes[brush],
                );
                x += step;
                up = !up;
                n += 1;
            }
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
        for &c in row.carets {
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
        let bands = self.blit_bands();
        self.bands.clear();
        self.shift_region = None;
        self.blit(hwnd, &bands)
    }

    /// Ce qu'il faut recopier vers la fenêtre : les bandes redessinées et, après un défilement, la
    /// zone dont les pixels ont été déplacés.
    fn blit_bands(&self) -> Vec<Band> {
        let mut v = self.bands.clone();
        v.extend(self.shift_region);
        v
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
        self.shift_region = None;
        self.blit(hwnd, &all)
    }

    fn blit(&self, hwnd: isize, bands: &[Band]) -> Result<()> {
        if bands.is_empty() {
            return Ok(());
        }
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: le DC de la fenêtre est obtenu puis libéré dans cette fonction.
        unsafe {
            let dc = GetDC(Some(hwnd));
            let result = self.blit_to_dc(dc, bands);
            ReleaseDC(Some(hwnd), dc);
            result
        }
    }

    /// Copie les bandes de la DIB vers `dc` (la fenêtre, ou une cible de test).
    fn blit_to_dc(&self, dc: HDC, bands: &[Band]) -> Result<()> {
        let w = self.size.0 as i32;
        for b in bands {
            // SAFETY: `dc` est un DC valide fourni par l'appelant ; `mem_dc` contient la DIB.
            unsafe {
                BitBlt(
                    dc,
                    0,
                    b.top,
                    w,
                    b.bottom - b.top,
                    Some(self.mem_dc),
                    0,
                    b.top,
                    SRCCOPY,
                )?;
            }
        }
        Ok(())
    }

    /// Couleurs utilisées.
    #[must_use]
    pub fn colors(&self) -> Colors {
        self.colors
    }

    /// Position (UTF-16) dans `text` la plus proche de l'abscisse `x` (pixels, relative à la zone
    /// cliente) : sert à placer le curseur à la souris.
    ///
    /// # Errors
    /// Échec DirectWrite.
    pub fn column_at(&mut self, text: &str, x: f32) -> Result<usize> {
        let hash = hash_bytes(text.as_bytes());
        let Some((layout, units)) = self.layout_for(text, hash)? else {
            return Ok(0);
        };
        let (mut trailing, mut inside) = (
            windows::core::BOOL::default(),
            windows::core::BOOL::default(),
        );
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: les pointeurs visent des variables locales valides ; la mise en page est vivante.
        unsafe {
            layout.HitTestPoint(
                x - self.metrics.pad_x,
                1.0,
                &mut trailing,
                &mut inside,
                &mut m,
            )?;
        }
        let pos = m.textPosition as usize
            + if trailing.as_bool() {
                m.length as usize
            } else {
                0
            };
        Ok(pos.min(units))
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
            tab_bar: m.height.ceil() as i32 + (10.0 * scale).round() as i32,
            top: 0,
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
        carets: Vec<Vec<usize>>,
        sels: Vec<Vec<(usize, usize)>>,
    }

    impl Rows {
        fn new(
            texts: Vec<String>,
            caret: Option<(usize, usize)>,
            selection: Option<(usize, usize, usize)>,
        ) -> Self {
            let mut r = Self {
                carets: vec![Vec::new(); texts.len()],
                sels: vec![Vec::new(); texts.len()],
                texts,
            };
            r.set_caret(caret);
            r.set_selection(selection);
            r
        }
        fn set_texts(&mut self, texts: Vec<String>) {
            self.carets.resize(texts.len(), Vec::new());
            self.sels.resize(texts.len(), Vec::new());
            self.texts = texts;
        }
        fn set_caret(&mut self, caret: Option<(usize, usize)>) {
            self.carets.iter_mut().for_each(Vec::clear);
            if let Some((r, c)) = caret {
                self.carets[r].push(c);
            }
        }
        fn set_selection(&mut self, selection: Option<(usize, usize, usize)>) {
            self.sels.iter_mut().for_each(Vec::clear);
            if let Some((r, a, b)) = selection {
                self.sels[r].push((a, b));
            }
        }
    }

    impl RowSource for Rows {
        fn len(&self) -> usize {
            self.texts.len()
        }
        fn row(&self, i: usize) -> Row<'_> {
            Row {
                text: &self.texts[i],
                selections: &self.sels[i],
                carets: &self.carets[i],
                underline: None,
                spans: &[],
                squiggles: &[],
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
        let mut state = Rows::new(lines(first, rows_n), Some((2, 3)), None);
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
                    state.set_texts(lines(first, rows_n));
                    scrolled = d as isize;
                }
                2 => {
                    // défilement vers le haut
                    let d = (next() % 4 + 1) as usize;
                    first -= d;
                    state.set_texts(lines(first, rows_n));
                    scrolled = -(d as isize);
                }
                3 => {
                    state.set_caret(Some((
                        (next() % rows_n as u64) as usize,
                        (next() % 10) as usize,
                    )));
                }
                _ => {
                    let r = (next() % rows_n as u64) as usize;
                    state.set_selection(Some((
                        r,
                        (next() % 5) as usize,
                        5 + (next() % 10) as usize,
                    )));
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
        let mut s = Rows::new(lines(0, n), Some((3, 0)), None);
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
        let s = Rows::new(vec!["Hello".into(), String::new()], None, None);
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
        let s = Rows::new(lines(0, 3), Some((0, 0)), None);
        r.draw(&s, 0).unwrap();
    }

    #[test]
    fn ligne_geante_tronquee_sans_planter() {
        let mut r = renderer();
        let s = Rows::new(vec!["é".repeat(1_000_000)], Some((0, 5)), Some((0, 0, 20)));
        r.draw(&s, 0).unwrap();
    }

    /// Source à une seule rangée avec des plages colorées.
    struct Colored<'a> {
        text: &'a str,
        spans: &'a [(usize, usize, u8)],
    }

    impl RowSource for Colored<'_> {
        fn len(&self) -> usize {
            1
        }
        fn row(&self, _: usize) -> Row<'_> {
            Row {
                text: self.text,
                spans: self.spans,
                ..Row::default()
            }
        }
    }

    /// Source à une seule rangée avec des soulignements.
    struct Squiggled<'a> {
        squiggles: &'a [(usize, usize, u8)],
    }

    impl RowSource for Squiggled<'_> {
        fn len(&self) -> usize {
            1
        }
        fn row(&self, _: usize) -> Row<'_> {
            Row {
                text: "let mauvaise_ligne = 1;",
                squiggles: self.squiggles,
                ..Row::default()
            }
        }
    }

    #[test]
    fn les_soulignements_s_appliquent_et_se_retirent() {
        let mut r = renderer();
        r.draw(&Squiggled { squiggles: &[] }, 0).unwrap();
        let plain = r.pixels().to_vec();
        r.draw(
            &Squiggled {
                squiggles: &[(4, 18, 0)],
            },
            0,
        )
        .unwrap();
        let marked = r.pixels().to_vec();
        assert_ne!(plain, marked, "le soulignement n'a pas été dessiné");
        // chaque gravité a sa couleur
        r.draw(
            &Squiggled {
                squiggles: &[(4, 18, 1)],
            },
            0,
        )
        .unwrap();
        assert_ne!(r.pixels(), marked.as_slice());
        // plage vide : un soulignement d'un caractère ; hors du texte : rien ne plante
        r.draw(
            &Squiggled {
                squiggles: &[(5, 5, 2), (90, 99, 0)],
            },
            0,
        )
        .unwrap();
        // retour à l'état sans soulignement : pixels identiques à l'image d'origine
        r.draw(&Squiggled { squiggles: &[] }, 0).unwrap();
        assert_eq!(r.pixels(), plain.as_slice());
    }

    #[test]
    fn les_couleurs_s_appliquent_et_se_retirent() {
        let text = "fn main() // x";
        let mut r = renderer();
        r.draw(&Colored { text, spans: &[] }, 0).unwrap();
        let plain = r.pixels().to_vec();

        // avec des plages : l'image change (le cache de mise en page est partagé par le texte)
        let spans = [(0, 2, 4u8), (10, 14, 1u8)];
        r.draw(
            &Colored {
                text,
                spans: &spans,
            },
            0,
        )
        .unwrap();
        let colored = r.pixels().to_vec();
        assert_ne!(plain, colored, "les couleurs n'ont pas été dessinées");

        // une couleur ne doit apparaître que sur la plage : la fin du texte reste la couleur de texte
        let mut full = renderer();
        full.draw(
            &Colored {
                text,
                spans: &spans,
            },
            0,
        )
        .unwrap();
        assert_eq!(full.pixels(), colored.as_slice(), "rendu non déterministe");

        // retour au texte sans plages : pixels identiques à l'image d'origine
        r.draw(&Colored { text, spans: &[] }, 0).unwrap();
        assert_eq!(
            r.pixels(),
            plain.as_slice(),
            "les couleurs n'ont pas été retirées"
        );
    }

    fn tab(label: &str, modified: bool, active: bool) -> TabInfo<'_> {
        TabInfo {
            label,
            modified,
            active,
        }
    }

    #[test]
    fn barre_d_onglets_regions_identiques_au_redessin_complet() {
        let tabs = [
            tab("main.rs", false, true),
            tab(
                "un_nom_de_fichier_vraiment_tres_long_pour_tronquer.txt",
                true,
                false,
            ),
        ];
        let mut inc = renderer();
        let mut full = renderer();
        inc.set_tabs(&tabs);
        full.set_tabs(&tabs);
        let m = inc.metrics();
        assert!(m.top > 0 && m.top == m.tab_bar);
        let rows_n = m.rows_for(300);
        assert!(rows_n > 5 && rows_n < renderer().metrics().rows_for(300));

        let mut first = 50usize;
        let mut state = Rows::new(lines(first, rows_n), Some((1, 2)), None);
        inc.draw(&state, 0).unwrap();
        for step in 0..60 {
            let mut scrolled = 0isize;
            match step % 4 {
                0 => state.texts[3].push('x'),
                1 => {
                    first += 2;
                    state.set_texts(lines(first, rows_n));
                    scrolled = 2;
                }
                2 => {
                    first -= 3;
                    state.set_texts(lines(first, rows_n));
                    scrolled = -3;
                }
                _ => state.set_caret(Some((step % rows_n, 1))),
            }
            // un changement d'onglet ne redessine que la barre
            if step % 9 == 0 {
                let active = (step / 9) % 2 == 0;
                let t = [
                    tab("main.rs", false, active),
                    tab("autre.py", step % 2 == 0, !active),
                ];
                inc.set_tabs(&t);
                full.set_tabs(&t);
            }
            inc.draw(&state, scrolled).unwrap();
            full.invalidate();
            full.draw(&state, 0).unwrap();
            assert!(
                inc.pixels() == full.pixels(),
                "pixels différents à l'étape {step}"
            );
        }
    }

    #[test]
    fn changer_un_onglet_ne_redessine_que_la_barre() {
        let mut r = renderer();
        r.set_tabs(&[tab("a.rs", false, true), tab("b.rs", false, false)]);
        let n = r.metrics().rows_for(300);
        let s = Rows::new(lines(0, n), None, None);
        r.draw(&s, 0).unwrap();
        r.set_tabs(&[tab("a.rs", true, true), tab("b.rs", false, false)]);
        let bar = r.metrics().tab_bar;
        assert_eq!(
            r.draw(&s, 0).unwrap().to_vec(),
            vec![Band {
                top: 0,
                bottom: bar
            }]
        );
        // rien ne change : plus rien à dessiner
        r.set_tabs(&[tab("a.rs", true, true), tab("b.rs", false, false)]);
        assert!(r.draw(&s, 0).unwrap().is_empty());
        // supprimer la barre redessine tout
        r.set_tabs(&[]);
        assert_eq!(r.metrics().top, 0);
        assert_eq!(r.draw(&s, 0).unwrap().len(), 1);
    }

    #[test]
    fn clic_sur_les_onglets() {
        let mut r = renderer();
        assert_eq!(r.tab_at(5.0, 5.0), None, "pas de barre");
        r.set_tabs(&[tab("a.rs", false, true), tab("b.rs", true, false)]);
        let bar = r.metrics().tab_bar as f32;
        // sous la barre : pas un onglet
        assert_eq!(r.tab_at(5.0, bar + 1.0), None);
        let (i0, close0) = r.tab_at(5.0, 5.0).unwrap();
        assert_eq!((i0, close0), (0, false));
        // la fin du premier onglet est sa zone de fermeture ; juste après commence le second
        let geo = r.tab_geometry();
        let end0 = geo[0].1 as f32;
        assert_eq!(r.tab_at(end0 - 3.0, 5.0), Some((0, true)));
        assert_eq!(r.tab_at(end0 + 3.0, 5.0), Some((1, false)));
        assert_eq!(r.tab_at(geo[1].1 as f32 + 50.0, 5.0), None);
    }

    fn overlay(selected: usize, query: &str, n_items: usize) -> Overlay {
        let mut rows = vec![OverlayRow {
            kind: OverlayRowKind::Input,
            text: format!("> {query}"),
            caret: Some(2 + query.chars().count()),
            ..OverlayRow::default()
        }];
        for k in 0..n_items {
            rows.push(OverlayRow {
                kind: if k == selected {
                    OverlayRowKind::Selected
                } else {
                    OverlayRowKind::Item
                },
                text: format!("mod_{k:03}/sub_05/file_00{k:02}.rs"),
                hint: "Ctrl+P".into(),
                caret: None,
                highlights: vec![(0, 3)],
            });
        }
        Overlay { rows }
    }

    #[test]
    fn panneau_regions_identiques_au_redessin_complet() {
        let mut inc = renderer();
        let mut full = renderer();
        let n = inc.metrics().rows_for(300);
        let mut state = Rows::new(lines(0, n), Some((8, 2)), None);
        inc.draw(&state, 0).unwrap();

        for step in 0..40 {
            let mut scrolled = 0isize;
            let ov = match step % 6 {
                0 => Some(overlay(0, "a", 4)),
                1 => Some(overlay(2, "ab", 4)),
                2 => Some(overlay(1, "ab", 2)),
                3 => None,
                4 => Some(overlay(0, "", 0)),
                _ => Some(overlay(3, "abc", 4)),
            };
            match step % 4 {
                0 => state.texts[3].push('x'),
                1 => state.set_caret(Some((step % n, step % 5))),
                2 => {
                    state.set_texts(lines(10 + step, n));
                    scrolled = 1;
                }
                _ => state.texts[6].push('y'),
            }
            inc.set_overlay(ov.as_ref());
            full.set_overlay(ov.as_ref());
            inc.draw(&state, scrolled).unwrap();
            full.invalidate();
            full.draw(&state, 0).unwrap();
            assert!(
                inc.pixels() == full.pixels(),
                "pixels différents à l'étape {step}"
            );
        }
    }

    #[test]
    fn deplacer_la_selection_ne_redessine_que_deux_rangees() {
        let mut r = renderer();
        let n = r.metrics().rows_for(300);
        let s = Rows::new(lines(0, n), None, None);
        r.set_overlay(Some(&overlay(0, "a", 4)));
        r.draw(&s, 0).unwrap();
        // l'élément sélectionné passe de la rangée 0 à la rangée 1 : deux rangées d'éditeur
        // (2 et 3) changent, contiguës donc une seule bande
        r.set_overlay(Some(&overlay(1, "a", 4)));
        let lh = r.metrics().line_height;
        assert_eq!(
            r.draw(&s, 0).unwrap().to_vec(),
            vec![Band {
                top: 2 * lh,
                bottom: 4 * lh
            }]
        );
        // fermer le panneau repeint toutes les rangées qu'il recouvrait (1 à 5)
        r.set_overlay(None);
        assert_eq!(
            r.draw(&s, 0).unwrap().to_vec(),
            vec![Band {
                top: lh,
                bottom: 6 * lh
            }]
        );
        // aucun changement : rien à faire
        assert!(r.draw(&s, 0).unwrap().is_empty());
    }

    #[test]
    fn clic_dans_le_panneau() {
        let mut r = renderer();
        assert_eq!(r.overlay_row_at(300.0, 40.0), None, "pas de panneau");
        r.set_overlay(Some(&overlay(0, "", 3)));
        let lh = r.metrics().line_height as f32;
        // rangée 1 de l'éditeur = rangée 0 du panneau (saisie), rangée 2 = premier résultat
        assert_eq!(r.overlay_row_at(320.0, lh * 1.5), Some(0));
        assert_eq!(r.overlay_row_at(320.0, lh * 2.5), Some(1));
        assert_eq!(r.overlay_row_at(320.0, lh * 4.5), Some(3));
        // au-dessus, en dessous et à gauche du panneau
        assert_eq!(r.overlay_row_at(320.0, lh * 0.5), None);
        assert_eq!(r.overlay_row_at(320.0, lh * 5.5), None);
        assert_eq!(r.overlay_row_at(2.0, lh * 2.5), None);
        assert!(r.overlay_columns() >= 20);
    }

    /// Cible de test qui joue le rôle de la fenêtre : un DC mémoire avec sa propre DIB.
    struct FakeWindow {
        dc: HDC,
        dib: HBITMAP,
        old: HGDIOBJ,
        bits: *mut u32,
        len: usize,
    }

    impl FakeWindow {
        fn new(size: (u32, u32)) -> Self {
            unsafe {
                let dc = CreateCompatibleDC(None);
                let (dib, old, bits) = make_dib(dc, size.0, size.1).unwrap();
                Self {
                    dc,
                    dib,
                    old,
                    bits,
                    len: size.0 as usize * size.1 as usize,
                }
            }
        }
        fn pixels(&self) -> &[u32] {
            unsafe { std::slice::from_raw_parts(self.bits, self.len) }
        }
        /// Comme `SoftRenderer::present`, mais vers cette fausse fenêtre.
        fn present(&self, r: &mut SoftRenderer) {
            let bands = r.blit_bands();
            r.bands.clear();
            r.shift_region = None;
            r.blit_to_dc(self.dc, &bands).unwrap();
        }
    }

    impl Drop for FakeWindow {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.old);
                let _ = DeleteObject(self.dib.into());
                let _ = DeleteDC(self.dc);
            }
        }
    }

    /// Ce que reçoit la fenêtre (bandes recopiées image après image) doit toujours être
    /// identique à la DIB, y compris après un défilement : les pixels déplacés dans la DIB
    /// doivent être recopiés eux aussi.
    #[test]
    fn la_fenetre_recoit_tout_ce_qui_change_dans_la_dib() {
        let mut r = renderer();
        let window = FakeWindow::new(r.size());
        r.set_tabs(&[tab("a.rs", false, true), tab("b.rs", true, false)]);
        let n = r.metrics().rows_for(300);
        let mut first = 40usize;
        let mut state = Rows::new(lines(first, n), Some((3, 1)), None);
        r.draw(&state, 0).unwrap();
        window.present(&mut r);
        assert!(window.pixels() == r.pixels(), "image initiale");

        for step in 0..60 {
            let mut scrolled = 0isize;
            match step % 6 {
                0 => {
                    first += 3;
                    state.set_texts(lines(first, n));
                    scrolled = 3;
                }
                1 => state.texts[4].push('z'),
                2 => {
                    first -= 2;
                    state.set_texts(lines(first, n));
                    scrolled = -2;
                }
                3 => state.set_caret(Some((step % n, 2))),
                4 => {
                    first += 1;
                    state.set_texts(lines(first, n));
                    scrolled = 1;
                }
                _ => {}
            }
            let ov = (step % 12 >= 8).then(|| overlay(1, "ab", 3));
            r.set_overlay(ov.as_ref());
            r.draw(&state, scrolled).unwrap();
            window.present(&mut r);
            assert!(
                window.pixels() == r.pixels(),
                "la fenêtre diffère de la DIB à l'étape {step} (défilement {scrolled})"
            );
        }
    }

    /// Coût réel d'une image de défilement (dessin + copie de la zone déplacée) sur une cible
    /// qui n'est pas rognée par l'écran. Mesure, pas test : `cargo test -p paulwrite-render
    /// mesure_defilement -- --ignored --nocapture`.
    #[test]
    #[ignore = "mesure"]
    fn mesure_defilement() {
        for size in [(1100u32, 720u32), (1920, 1080), (3840, 2160)] {
            let mut r = SoftRenderer::new(1.0, size, Colors::default()).unwrap();
            let window = FakeWindow::new(size);
            r.set_tabs(&[tab("a.rs", false, true)]);
            let n = r.metrics().rows_for(size.1);
            let mut first = 5000usize;
            let mut state = Rows::new(lines(first, n), None, None);
            r.draw(&state, 0).unwrap();
            window.present(&mut r);
            let mut times = Vec::new();
            for step in 0..200 {
                let d: isize = if step % 2 == 0 { 3 } else { -3 };
                first = (first as isize + d) as usize;
                state.set_texts(lines(first, n));
                let t = std::time::Instant::now();
                r.draw(&state, d).unwrap();
                window.present(&mut r);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{}x{} : défilement de 3 lignes, dessin + copie : p50 {:.2} ms | p95 {:.2} | max {:.2}",
                size.0,
                size.1,
                times[times.len() / 2],
                times[times.len() * 95 / 100],
                times[times.len() - 1]
            );
        }
    }
}
