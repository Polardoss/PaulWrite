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
use windows_numerics::{Matrix3x2, Vector2};

/// Marge entre la gouttière et le texte, en pixels logiques (multipliée par l'échelle DPI).
const TEXT_PAD: f32 = 6.0;
/// Marge à gauche des numéros de ligne.
const GUTTER_LEFT: f32 = 16.0;
/// Espace à droite des numéros (réservé aux flèches de repli, comme Sublime Text).
const GUTTER_FOLD: f32 = 19.0;
/// Hauteur de la barre d'onglets et de la barre d'état, en pixels logiques.
const TAB_BAR_H: f32 = 33.0;
const STATUS_BAR_H: f32 = 25.0;
/// Largeur de la barre de défilement (à l'extrême droite).
const SCROLLBAR_W: f32 = 12.0;
/// Largeur maximale d'une ligne affichée, en unités UTF-16 (au-delà : tronquée à l'affichage
/// pour qu'une ligne de plusieurs Mo ne fige pas DirectWrite).
const MAX_UNITS: usize = 8192;
/// Nombre de mises en page conservées avant vidage du cache.
const MAX_CACHED_LAYOUTS: usize = 1024;
/// Taille de la police du texte, en pixels logiques.
const FONT_PX: f32 = 15.0;

mod chrome;
pub use chrome::{
    Chrome, MinimapSource, Scroll, Sidebar, SidebarKind, SidebarRow, StripHit, TabButton,
};

/// Pinceaux : 0 texte, 1 fond, 2 sélection, 3 curseur, puis `3 + catégorie` (1..`KIND_SLOTS`) ;
/// ensuite ceux de l'interface (voir `CHROME_COLORS`), dans l'ordre.
const BR_BG: usize = 1;
const BR_CHROME: usize = 3 + KIND_SLOTS;
const BR_TAB_BAR: usize = BR_CHROME;
const BR_TAB_TEXT: usize = BR_CHROME + 1;
const BR_TAB_TEXT_ACTIVE: usize = BR_CHROME + 2;
const BR_TAB_BUTTON: usize = BR_CHROME + 3;
const BR_TAB_CLOSE: usize = BR_CHROME + 4;
const BR_GUTTER_NUM: usize = BR_CHROME + 5;
const BR_GUTTER_CUR_BG: usize = BR_CHROME + 6;
const BR_GUTTER_CUR_NUM: usize = BR_CHROME + 7;
const BR_INDENT: usize = BR_CHROME + 8;
const BR_STATUS_BG: usize = BR_CHROME + 9;
const BR_STATUS_TEXT: usize = BR_CHROME + 10;
const BR_STATUS_ICON: usize = BR_CHROME + 11;
const BR_PANEL_INPUT: usize = BR_CHROME + 12;
const BR_PANEL_BG: usize = BR_CHROME + 13;
const BR_PANEL_SEL: usize = BR_CHROME + 14;
const BR_PANEL_TEXT: usize = BR_CHROME + 15;
const BR_PANEL_HINT: usize = BR_CHROME + 16;
const BR_PANEL_NOTE: usize = BR_CHROME + 17;
const BR_SCROLL_TRACK: usize = BR_CHROME + 18;
const BR_SCROLL_THUMB: usize = BR_CHROME + 19;
/// Soulignements des diagnostics : erreur, avertissement, information (3 pinceaux consécutifs).
const BR_PANEL_SEL_TEXT: usize = BR_CHROME + 20;
const BR_PANEL_THUMB: usize = BR_CHROME + 21;
const BR_SIDEBAR_BG: usize = BR_CHROME + 22;
const BR_SIDEBAR_SEL: usize = BR_CHROME + 23;
const BR_SIDEBAR_TEXT: usize = BR_CHROME + 24;
const BR_SIDEBAR_TEXT_SEL: usize = BR_CHROME + 25;
const BR_SIDEBAR_DIM: usize = BR_CHROME + 26;
const BR_SQUIGGLE: usize = BR_CHROME + 27;

/// Couleurs de l'interface (thème « Mariana » de Sublime Text, mesurées sur une capture), dans
/// l'ordre des `BR_*` ci-dessus.
const CHROME_COLORS: [[f32; 3]; 30] = [
    rgb8(0x72, 0x78, 0x7e), // barre d'onglets
    rgb8(0xc6, 0xc9, 0xcb), // onglet inactif : texte
    rgb8(0xff, 0xff, 0xff), // onglet actif : texte
    rgb8(0xc6, 0xc9, 0xcb), // boutons de la barre d'onglets
    rgb8(0x8c, 0x91, 0x96), // croix de fermeture
    rgb8(0x84, 0x8b, 0x95), // numéros de ligne
    rgb8(0x4c, 0x58, 0x63), // gouttière : ligne du curseur (fond)
    rgb8(0xc0, 0xc5, 0xce), // gouttière : ligne du curseur (numéro)
    rgb8(0x3d, 0x46, 0x4f), // guides d'indentation
    rgb8(0xc7, 0xcb, 0xd1), // barre d'état : fond
    rgb8(0x40, 0x40, 0x40), // barre d'état : texte
    rgb8(0x76, 0x79, 0x7c), // barre d'état : icône
    rgb8(0xff, 0xff, 0xff), // panneau : saisie (fond)
    rgb8(0xe3, 0xe5, 0xe8), // panneau : liste (fond)
    rgb8(0xc7, 0xcb, 0xd1), // panneau : sélection
    rgb8(0x44, 0x4b, 0x54), // panneau : texte
    rgb8(0x44, 0x4b, 0x54), // panneau : raccourci
    rgb8(0x6b, 0x72, 0x7a), // panneau : message
    rgb8(0x44, 0x4b, 0x53), // barre de défilement : rail
    rgb8(0x69, 0x6f, 0x75), // barre de défilement : curseur
    rgb8(0x2d, 0x32, 0x39), // panneau : texte de la sélection
    rgb8(0xb3, 0xb7, 0xbc), // panneau : barre de défilement
    rgb8(0x2a, 0x31, 0x39), // barre latérale : fond
    rgb8(0x3a, 0x44, 0x4e), // barre latérale : sélection
    rgb8(0xb7, 0xbf, 0xc9), // barre latérale : texte
    rgb8(0xff, 0xff, 0xff), // barre latérale : texte sélectionné
    rgb8(0x7d, 0x86, 0x90), // barre latérale : titre et flèches
    rgb8(0xec, 0x5f, 0x67), // erreur
    rgb8(0xfa, 0xc8, 0x63), // avertissement
    rgb8(0x66, 0x99, 0xcc), // information
];

const fn rgb8(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

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
            background: rgb8(0x30, 0x38, 0x41),
            text: rgb8(0xd8, 0xde, 0xe9),
            selection: rgb8(0x4e, 0x5a, 0x65),
            caret: rgb8(0xf9, 0xae, 0x58),
            kinds: DEFAULT_KINDS,
        }
    }
}

/// Palette par défaut (thème « Mariana »). Ordre = catégories du crate `paulwrite-syntax` :
/// 1 commentaire, 2 chaîne, 3 nombre, 4 mot-clé, 5 fonction, 6 type, 7 constante, 8 propriété,
/// 9 opérateur, 10 ponctuation, 11 attribut/macro, 12 balise, 13 échappement, 14 prédéfini.
const DEFAULT_KINDS: [[f32; 3]; KIND_SLOTS] = [
    rgb8(0xd8, 0xde, 0xe9),
    rgb8(0xa7, 0xad, 0xba),
    rgb8(0x99, 0xc7, 0x94),
    rgb8(0xf9, 0xae, 0x58),
    rgb8(0xc6, 0x95, 0xc6),
    rgb8(0x66, 0x99, 0xcc),
    rgb8(0xfa, 0xc8, 0x63),
    rgb8(0xec, 0x5f, 0x67),
    rgb8(0xd8, 0xde, 0xe9),
    rgb8(0xf9, 0x7b, 0x58),
    rgb8(0xd8, 0xde, 0xe9),
    rgb8(0x5f, 0xb3, 0xb3),
    rgb8(0xec, 0x5f, 0x67),
    rgb8(0x5f, 0xb3, 0xb3),
    rgb8(0x66, 0x99, 0xcc),
    rgb8(0xd8, 0xde, 0xe9),
];

/// Dimensions d'un caractère à l'échelle courante, en pixels physiques.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Largeur d'un caractère (police à chasse fixe).
    pub advance: f32,
    /// Hauteur d'une rangée.
    pub line_height: i32,
    /// Abscisse du texte (gouttière comprise) : le caractère 0 d'une rangée commence ici.
    pub pad_x: f32,
    /// Hauteur de la barre d'onglets lorsqu'elle est affichée.
    pub tab_bar: i32,
    /// Ordonnée de la première rangée : 0 sans onglets, `tab_bar` avec.
    pub top: i32,
    /// Hauteur de la barre d'état (en bas).
    pub status_bar: i32,
    /// Largeur de la gouttière (numéros de ligne) ; 0 si elle est masquée.
    pub gutter: i32,
    /// Largeur de la zone de droite (minimap et barre de défilement) ; 0 si elle est masquée.
    pub minimap: i32,
    /// Largeur de la barre latérale (à gauche de tout, onglets compris) ; 0 si elle est masquée.
    pub sidebar: i32,
}

impl Metrics {
    /// Nombre de rangées **entières** qui tiennent dans `height` pixels (entre la barre d'onglets
    /// et la barre d'état).
    #[must_use]
    pub fn rows_for(&self, height: u32) -> usize {
        ((height as i32 - self.top - self.status_bar).max(0) / self.line_height.max(1)) as usize
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
    /// Barre de défilement de la liste : `(première rangée affichée, rangées affichées, total)`.
    pub scroll: Option<(usize, usize, usize)>,
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
    /// Numéro de ligne affiché dans la gouttière (à partir de 1) ; 0 : aucun (au-delà de la fin).
    pub number: u32,
    /// Ligne du curseur principal : sa gouttière est mise en évidence.
    pub current: bool,
}

/// Source des rangées visibles (évite d'allouer un tableau par image).
pub trait RowSource {
    fn len(&self) -> usize;
    fn row(&self, index: usize) -> Row<'_>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Rectangle de pixels `[left, right) x [top, bottom)` redessiné.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}

impl Band {
    /// Bande pleine largeur.
    #[must_use]
    pub fn rows(top: i32, bottom: i32, width: i32) -> Self {
        Self {
            left: 0,
            right: width,
            top,
            bottom,
        }
    }

    fn is_empty(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    fn intersects(&self, o: &Band) -> bool {
        self.left < o.right && o.left < self.right && self.top < o.bottom && o.top < self.bottom
    }
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
    mix(row.number as usize);
    mix(usize::from(row.current));
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
    /// Polices de l'interface (proportionnelles) : onglets, texte discret, panneau.
    ui: [IDWriteTextFormat; 3],
    ui_layouts: HashMap<u64, chrome::UiLayout>,
    /// Voir `BR_*` pour l'ordre des pinceaux.
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
    // Gouttière, barre d'état
    status: chrome::Status,
    status_clean: bool,
    // Minimap et barre de défilement (zone de droite)
    minimap: chrome::MinimapState,
    strip_dirty: bool,
    // Barre latérale (arborescence du dossier ouvert)
    sidebar: Option<Sidebar>,
    sidebar_clean: bool,
    // Panneau flottant
    overlay: Option<Overlay>,
    overlay_hashes: Vec<u64>,
    /// Ce qui est actuellement dessiné du panneau : rectangle et empreintes des rangées.
    drawn_overlay: Option<(Band, Vec<u64>)>,
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
            let ui = make_ui_formats(&dwrite, scale)?;

            let mut brushes = Vec::with_capacity(BR_SQUIGGLE + 3);
            for c in [
                colors.text,
                colors.background,
                colors.selection,
                colors.caret,
            ]
            .into_iter()
            .chain(colors.kinds.iter().skip(1).copied())
            .chain(CHROME_COLORS)
            {
                brushes.push(
                    rt.CreateSolidColorBrush(&rgb(c), None)?
                        .cast::<ID2D1Brush>()?,
                );
            }
            debug_assert_eq!(brushes.len(), BR_SQUIGGLE + 3);

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
                ui,
                ui_layouts: HashMap::new(),
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
                status: chrome::Status::default(),
                status_clean: false,
                minimap: chrome::MinimapState::default(),
                strip_dirty: false,
                sidebar: None,
                sidebar_clean: false,
                overlay: None,
                overlay_hashes: Vec::new(),
                drawn_overlay: None,
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
        self.minimap = chrome::MinimapState::default();
        self.refresh_metrics();
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
        self.ui = unsafe { make_ui_formats(&self.dwrite, scale)? };
        self.format = format;
        self.metrics = metrics;
        self.scale = scale;
        self.refresh_metrics();
        self.layouts.clear();
        self.ui_layouts.clear();
        self.minimap = chrome::MinimapState::default();
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

    /// Recalcule la géométrie qui dépend de l'état (barre d'onglets, gouttière, zone de droite) ;
    /// un changement décale les rangées et force un redessin complet.
    fn refresh_metrics(&mut self) {
        let s = self.scale;
        let top = if self.tabs.is_empty() {
            0
        } else {
            self.metrics.tab_bar
        };
        let gutter = if self.status.digits == 0 {
            0
        } else {
            (GUTTER_LEFT * s + self.status.digits as f32 * self.metrics.advance + GUTTER_FOLD * s)
                .round() as i32
        };
        let minimap = if self.status.scroll.is_some() {
            (SCROLLBAR_W * s + self.minimap_width()).round() as i32
        } else {
            0
        };
        let sidebar = if self.sidebar.is_some() {
            (250.0 * s).round() as i32
        } else {
            0
        };
        let pad_x = (sidebar + gutter) as f32 + (TEXT_PAD * s).round();
        let m = &mut self.metrics;
        if (m.top, m.gutter, m.minimap, m.sidebar) != (top, gutter, minimap, sidebar)
            || (m.pad_x - pad_x).abs() > 0.1
        {
            m.top = top;
            m.gutter = gutter;
            m.minimap = minimap;
            m.sidebar = sidebar;
            m.pad_x = pad_x;
            self.valid = false;
        }
    }

    /// Largeur de la minimap (sans la barre de défilement) : ~8 % de la fenêtre, 150 px au plus,
    /// absente dans une fenêtre étroite.
    fn minimap_width(&self) -> f32 {
        let s = self.scale;
        let w = self.size.0 as f32 - self.metrics.sidebar as f32;
        if w < 560.0 * s {
            0.0
        } else {
            (w * 0.078).clamp(60.0 * s, 150.0 * s).round()
        }
    }

    /// Abscisses `[gauche, droite)` de la zone des rangées (entre le bord gauche et la zone de
    /// droite).
    fn editor_x(&self) -> (i32, i32) {
        (
            self.metrics.sidebar,
            self.size.0 as i32 - self.metrics.minimap,
        )
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
        let (ex0, ex1) = self.editor_x();
        let status_top = h - self.metrics.status_bar;

        self.cur.clear();
        for r in 0..rows {
            let row = src.row(r);
            self.cur.push(Some(Sig {
                hash: hash_bytes(row.text.as_bytes()),
                deco: deco_hash(&row),
            }));
        }
        self.bands.clear();

        // Décaler les pixels sous un panneau ouvert laisserait des résidus du panneau : redessin.
        if scrolled_by != 0 && self.drawn_overlay.is_some() {
            self.valid = false;
        }
        if self.valid {
            self.shift_for_scroll(scrolled_by, rows, lh, top, (ex0, ex1))?;
        } else {
            self.prev.clear();
        }

        self.dirty.clear();
        for r in 0..rows {
            let clean = self.valid && self.prev.get(r).copied().flatten() == self.cur[r];
            self.dirty.push(!clean);
        }

        let overlay_now = self.panel_geometry();
        if self.valid {
            if !self.tabs_clean && top > 0 {
                self.bands.push(Band {
                    left: ex0,
                    right: w,
                    top: 0,
                    bottom: top,
                });
            }
            if !self.sidebar_clean && ex0 > 0 {
                self.bands.push(Band {
                    left: 0,
                    right: ex0,
                    top: 0,
                    bottom: status_top,
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
                    left: ex0,
                    right: ex1,
                    top: top + start as i32 * lh,
                    bottom: (top + r as i32 * lh).min(h),
                });
            }
            if self.strip_dirty && ex1 < w {
                self.bands.push(Band {
                    left: ex1,
                    right: w,
                    top,
                    bottom: status_top,
                });
            }
            if !self.status_clean && self.metrics.status_bar > 0 {
                let band = self.status_band(status_top, h);
                self.bands.push(band);
            }
            self.overlay_bands(overlay_now.as_ref());
        } else {
            // Redessin complet : tout l'écran, y compris la marge sous la dernière rangée.
            self.bands.push(Band::rows(0, h, w));
        }

        let bands = std::mem::take(&mut self.bands);
        let result = self.paint_bands(&bands, src, rows);
        self.bands = bands;
        result?;
        self.tabs_clean = true;
        self.sidebar_clean = true;
        self.status_clean = true;
        self.status.right_dirty = false;
        self.strip_dirty = false;
        self.drawn_overlay = overlay_now.map(|(rect, _)| (rect, self.overlay_hashes.clone()));

        std::mem::swap(&mut self.prev, &mut self.cur);
        self.valid = true;
        Ok(&self.bands)
    }

    /// Déplace les pixels déjà dessinés et décale les signatures (défilement).
    fn shift_for_scroll(
        &mut self,
        scrolled_by: isize,
        rows: usize,
        lh: i32,
        top: i32,
        (ex0, ex1): (i32, i32),
    ) -> Result<()> {
        let d = scrolled_by.unsigned_abs();
        if d == 0 || d >= rows.min(self.prev.len()) || self.prev.is_empty() {
            return Ok(());
        }
        let dy = d as i32 * lh;
        let region = self.prev.len() as i32 * lh;
        let width = ex1 - ex0;
        self.shift_region = Some(Band {
            left: ex0,
            right: ex1,
            top,
            bottom: (top + region).min(self.size.1 as i32),
        });
        // SAFETY: copie de pixels à l'intérieur de la DIB sélectionnée dans `mem_dc` ; GDI gère
        // le chevauchement source/destination.
        unsafe {
            if scrolled_by > 0 {
                BitBlt(
                    self.mem_dc,
                    ex0,
                    top,
                    width,
                    region - dy,
                    Some(self.mem_dc),
                    ex0,
                    top + dy,
                    SRCCOPY,
                )?;
                self.prev.drain(0..d);
            } else {
                BitBlt(
                    self.mem_dc,
                    ex0,
                    top + dy,
                    width,
                    region - dy,
                    Some(self.mem_dc),
                    ex0,
                    top,
                    SRCCOPY,
                )?;
                self.prev.splice(0..0, std::iter::repeat(None).take(d));
            }
        }
        Ok(())
    }

    /// Redessine chaque bande **entièrement** (toutes les couches qui la recouvrent) : une bande
    /// ne dépend que de l'état courant, jamais de ce qui était dessiné avant.
    fn paint_bands(&mut self, bands: &[Band], src: &dyn RowSource, rows: usize) -> Result<()> {
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let lh = self.metrics.line_height.max(1);
        let top = self.metrics.top;
        let pad = self.metrics.pad_x;
        let (ex0, ex1) = self.editor_x();
        let status_top = h - self.metrics.status_bar;
        for band in bands {
            let b = Band {
                left: band.left.max(0),
                right: band.right.min(w),
                top: band.top.max(0),
                bottom: band.bottom.min(h),
            };
            if b.is_empty() {
                continue;
            }
            // Bande entièrement dans la zone de droite, entre les onglets et la barre d'état :
            // copie de pixels seulement (pas de Direct2D).
            if ex1 < w && b.left >= ex1 && b.top >= top && b.bottom <= status_top {
                self.blit_strip(&b);
                continue;
            }
            // On ne lie à la cible Direct2D que la bande modifiée : `BindDC` et `EndDraw`
            // copient toute la zone liée (lier l'écran entier coûtait ~5 ms pour une ligne).
            // Le dessin se fait en coordonnées de la fenêtre, une translation les ramène à la bande.
            let rect = RECT {
                left: b.left,
                top: b.top,
                right: b.right,
                bottom: b.bottom,
            };
            // SAFETY: appels Direct2D entre `BeginDraw` et `EndDraw` ; `rect` borne la DIB.
            unsafe {
                self.rt.BindDC(self.mem_dc, &rect)?;
                self.rt.BeginDraw();
                self.rt
                    .SetTransform(&Matrix3x2::translation(-(b.left as f32), -(b.top as f32)));
                self.rt.FillRectangle(
                    &D2D_RECT_F {
                        left: b.left as f32,
                        top: b.top as f32,
                        right: b.right as f32,
                        bottom: b.bottom as f32,
                    },
                    &self.brushes[BR_BG],
                );
                if b.top < top && b.right > ex0 {
                    self.paint_tabs(w)?;
                }
                if b.left < ex0 && b.top < status_top {
                    self.paint_sidebar(ex0, status_top)?;
                }
                if b.bottom > top && b.top < status_top && rows > 0 {
                    let r0 = ((b.top - top).max(0) / lh) as usize;
                    let r1 = ((b.bottom - top + lh - 1) / lh) as usize;
                    for row_idx in r0..r1.min(rows) {
                        let row = src.row(row_idx);
                        let y = (top + row_idx as i32 * lh) as f32;
                        self.paint_row(&row, pad, y, lh as f32)?;
                    }
                }
                if b.bottom > status_top && self.metrics.status_bar > 0 {
                    self.paint_status(w, status_top, h)?;
                }
                self.paint_overlay(&b)?;
                self.rt.EndDraw(None, None)?;
                self.blit_strip(&b);
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
        self.paint_gutter(row, y, lh)?;
        self.paint_indent_guides(row.text, pad, y, lh);
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
        let all = [Band::rows(0, self.size.1 as i32, self.size.0 as i32)];
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
        for b in bands {
            // SAFETY: `dc` est un DC valide fourni par l'appelant ; `mem_dc` contient la DIB.
            unsafe {
                BitBlt(
                    dc,
                    b.left,
                    b.top,
                    b.right - b.left,
                    b.bottom - b.top,
                    Some(self.mem_dc),
                    b.left,
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
            pad_x: (TEXT_PAD * scale).round(),
            tab_bar: (TAB_BAR_H * scale).round() as i32,
            top: 0,
            status_bar: (STATUS_BAR_H * scale).round() as i32,
            gutter: 0,
            minimap: 0,
            sidebar: 0,
        },
    ))
}

/// Polices de l'interface (Segoe UI) : onglets, texte discret (barre d'état, raccourcis), panneau.
unsafe fn make_ui_formats(dwrite: &IDWriteFactory, scale: f32) -> Result<[IDWriteTextFormat; 3]> {
    let make = |px: f32| -> Result<IDWriteTextFormat> {
        let f = dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None,
            DWRITE_FONT_WEIGHT_REGULAR,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            px * scale,
            w!("en-us"),
        )?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(f)
    };
    Ok([make(12.0)?, make(11.0)?, make(13.0)?])
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
                number: number_of(&self.texts[i], i),
                current: i == 2,
            }
        }
    }

    /// Numéro de ligne d'une rangée de test : celui écrit dans « ligne N », sinon sa position.
    fn number_of(text: &str, row: usize) -> u32 {
        text.split("ligne ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse::<u32>().ok())
            .map_or(row as u32 + 1, |n| n + 1)
    }

    /// Document de test de la minimap.
    struct Doc;

    impl MinimapSource for Doc {
        fn len(&self) -> usize {
            200
        }
        fn line(&self, index: usize, out: &mut String) {
            out.push_str(&format!(
                "{}let x = {index}; // essai",
                " ".repeat(index % 4 * 4)
            ));
        }
    }

    fn chrome(left: &str, first: usize, visible: usize) -> Chrome<'_> {
        Chrome {
            line_digits: 4,
            status_left: left,
            status_right: &["Tab Size: 4", "Rust"],
            scroll: Some(Scroll {
                first,
                visible,
                total: 200,
            }),
        }
    }

    /// Moteur de test avec gouttière, barre d'état, minimap et barre de défilement.
    fn renderer() -> SoftRenderer {
        let mut r = SoftRenderer::new(1.0, (640, 300), Colors::default()).expect("renderer");
        let visible = r.metrics().rows_for(300);
        r.set_chrome(&chrome("Line 1, Column 1", 0, visible));
        r.set_minimap(&Doc, 1).unwrap();
        r
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
        let right = 640 - r.metrics().minimap;
        assert_eq!(
            bands,
            vec![Band {
                left: 0,
                right,
                top: 3 * lh,
                bottom: 4 * lh
            }]
        );
        // aucune modification : rien à redessiner
        assert!(r.draw(&s, 0).unwrap().is_empty());
    }

    #[test]
    fn le_texte_dessine_des_pixels_et_le_fond_est_uni() {
        // sans gouttière ni barre d'état : le dernier pixel est du fond
        let mut r = SoftRenderer::new(1.0, (640, 300), Colors::default()).expect("renderer");
        let s = Rows::new(vec!["Hello".into(), String::new()], None, None);
        r.draw(&s, 0).unwrap();
        let (w, _) = r.size();
        let lh = r.metrics().line_height as usize;
        let px = r.pixels();
        let bg = px[5 * lh * w as usize + w as usize - 1];
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
            vec![Band::rows(0, bar, 640)]
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
        let geo = r.tab_geometry();
        assert_eq!(
            r.tab_at(geo[0].0 as f32 - 3.0, 5.0),
            None,
            "boutons à gauche des onglets"
        );
        let (i0, close0) = r.tab_at(geo[0].0 as f32 + 5.0, 5.0).unwrap();
        assert_eq!((i0, close0), (0, false));
        // la fin du premier onglet est sa zone de fermeture ; juste après commence le second
        let end0 = geo[0].1 as f32;
        assert_eq!(r.tab_at(end0 - 3.0, 5.0), Some((0, true)));
        assert_eq!(r.tab_at(end0 + 3.0, 5.0), Some((1, false)));
        assert_eq!(r.tab_at(geo[1].1 as f32 + 50.0, 5.0), None);
    }

    #[test]
    fn la_gouttiere_decale_le_texte_et_affiche_les_numeros() {
        let mut with = renderer();
        let mut without = SoftRenderer::new(1.0, (640, 300), Colors::default()).unwrap();
        let n = with.metrics().rows_for(300);
        assert!(
            with.metrics().gutter > 40,
            "gouttière large pour 4 chiffres"
        );
        assert_eq!(without.metrics().gutter, 0);
        assert!(with.metrics().pad_x > without.metrics().pad_x + 40.0);
        let s = Rows::new(lines(0, n), None, None);
        with.draw(&s, 0).unwrap();
        without.draw(&s, 0).unwrap();
        // la ligne du curseur (rangée 2) a un fond de gouttière différent des autres
        let g = with.metrics().gutter as usize;
        let lh = with.metrics().line_height as usize;
        let top = with.metrics().top as usize;
        let at =
            |r: &SoftRenderer, row: usize, x: usize| r.pixels()[(top + row * lh + 1) * 640 + x];
        assert_ne!(
            at(&with, 2, 2),
            at(&with, 3, 2),
            "ligne courante mise en évidence"
        );
        assert_eq!(
            at(&with, 3, 2),
            at(&with, 3, g + 1),
            "autres lignes : fond de l'éditeur"
        );
        // des pixels de numéro dans la gouttière
        let bg = at(&with, 3, g + 1);
        let digits =
            (0..lh).any(|dy| (0..g).any(|x| with.pixels()[(top + 3 * lh + dy) * 640 + x] != bg));
        assert!(digits, "numéro de ligne dessiné");
    }

    #[test]
    fn la_barre_d_etat_ne_redessine_que_sa_bande() {
        let mut r = renderer();
        let n = r.metrics().rows_for(300);
        let s = Rows::new(lines(0, n), None, None);
        r.draw(&s, 0).unwrap();
        // le texte de gauche change (à chaque frappe) : seule la partie gauche est redessinée
        r.set_chrome(&chrome("Line 9, Column 2", 0, n));
        let (h, st) = (r.size().1 as i32, r.metrics().status_bar);
        let bands = r.draw(&s, 0).unwrap().to_vec();
        assert_eq!(bands.len(), 1);
        assert_eq!((bands[0].top, bands[0].bottom), (h - st, h));
        assert!(
            bands[0].left > 0 && bands[0].left < 40,
            "sans l'icône : {}",
            bands[0].left
        );
        assert!(
            bands[0].right > 100 && bands[0].right < 320,
            "texte de gauche seulement : {}",
            bands[0].right
        );
        r.set_chrome(&chrome("Line 9, Column 2", 0, n));
        assert!(r.draw(&s, 0).unwrap().is_empty(), "rien ne change");
        // un texte plus court : l'ancien est effacé
        r.set_chrome(&chrome("L1", 0, n));
        let band = r.draw(&s, 0).unwrap()[0];
        assert!(band.right > 100, "couvre l'ancien texte : {}", band.right);
        // un élément de droite change : toute la largeur
        let mut c = chrome("L1", 0, n);
        c.status_right = &["Tab Size: 2", "Rust"];
        r.set_chrome(&c);
        assert_eq!(
            r.draw(&s, 0).unwrap().to_vec(),
            vec![Band::rows(h - st, h, 640)]
        );
    }

    #[test]
    fn le_defilement_ne_redessine_que_la_zone_de_droite() {
        let mut r = renderer();
        let n = r.metrics().rows_for(300);
        let s = Rows::new(lines(0, n), None, None);
        r.draw(&s, 0).unwrap();
        // la barre de défilement bouge, le texte de la minimap ne change pas de page
        r.set_chrome(&chrome("Line 1, Column 1", 3, n));
        r.set_minimap(&Doc, 1).unwrap();
        let m = r.metrics();
        let h = r.size().1 as i32;
        assert_eq!(
            r.draw(&s, 0).unwrap().to_vec(),
            vec![Band {
                left: 640 - m.minimap,
                right: 640,
                top: m.top,
                bottom: h - m.status_bar
            }]
        );
        // un texte modifié reconstruit la minimap
        r.set_minimap(&Doc, 2).unwrap();
        assert_eq!(r.draw(&s, 0).unwrap().len(), 1);
        // clics : minimap, barre de défilement, hors zone
        let x_map = 640.0 - m.minimap as f32 + 5.0;
        let y = m.top as f32 + 40.0;
        assert!(matches!(
            r.strip_hit(x_map, y),
            Some(StripHit::Minimap { .. })
        ));
        assert!(matches!(
            r.strip_hit(639.0, y),
            Some(StripHit::Scrollbar { .. })
        ));
        assert_eq!(r.strip_hit(100.0, y), None);
        assert_eq!(r.strip_hit(x_map, h as f32 - 2.0), None, "barre d'état");
    }

    fn sidebar(selected: usize, n: usize) -> Sidebar {
        let mut rows = vec![SidebarRow {
            label: "Folders".into(),
            depth: 0,
            kind: SidebarKind::Header,
            selected: false,
        }];
        for i in 0..n {
            rows.push(SidebarRow {
                label: format!("fichier_{i}.rs"),
                depth: i % 3,
                kind: match i % 3 {
                    0 => SidebarKind::FolderClosed,
                    1 => SidebarKind::FolderOpen,
                    _ => SidebarKind::File,
                },
                selected: i == selected,
            });
        }
        Sidebar { rows }
    }

    #[test]
    fn la_barre_laterale_decale_tout_et_se_redessine_seule() {
        let mut r = renderer();
        let n = r.metrics().rows_for(300);
        let s = Rows::new(lines(0, n), None, None);
        r.draw(&s, 0).unwrap();
        let pad_without = r.metrics().pad_x;
        r.set_sidebar(Some(&sidebar(0, 5)));
        let m = r.metrics();
        assert!(m.sidebar > 100, "colonne de gauche réservée");
        assert!(
            (m.pad_x - pad_without - m.sidebar as f32).abs() < 1.0,
            "le texte est décalé"
        );
        // apparition : redessin complet
        assert_eq!(r.draw(&s, 0).unwrap().len(), 1);
        // le contenu change : seule la colonne de gauche est redessinée
        r.set_sidebar(Some(&sidebar(1, 5)));
        let h = r.size().1 as i32;
        assert_eq!(
            r.draw(&s, 0).unwrap().to_vec(),
            vec![Band {
                left: 0,
                right: m.sidebar,
                top: 0,
                bottom: h - m.status_bar
            }]
        );
        assert!(r.draw(&s, 0).unwrap().is_empty());
        // clics sur les lignes (24 px chacune) et hors de la colonne
        let row_h = (24.0 * 1.0) as f32;
        assert_eq!(r.sidebar_row_at(10.0, row_h * 2.5), Some(2));
        assert_eq!(
            r.sidebar_row_at(10.0, row_h * 7.5),
            None,
            "sous la dernière ligne"
        );
        assert_eq!(r.sidebar_row_at(m.sidebar as f32 + 5.0, 10.0), None);
        assert!(r.sidebar_capacity() >= 8);
        // masquer la barre rend la place
        r.set_sidebar(None);
        assert_eq!(r.metrics().sidebar, 0);
        assert!((r.metrics().pad_x - pad_without).abs() < 0.5);
    }

    #[test]
    fn barre_laterale_regions_identiques_au_redessin_complet() {
        let tabs = [tab("a.rs", false, true), tab("b.rs", true, false)];
        let mut inc = renderer();
        let mut full = renderer();
        inc.set_tabs(&tabs);
        full.set_tabs(&tabs);
        let n = inc.metrics().rows_for(300);
        let mut first = 20usize;
        let mut state = Rows::new(lines(first, n), Some((2, 1)), None);
        for step in 0..48 {
            let mut scrolled = 0isize;
            match step % 6 {
                0 => {
                    inc.set_sidebar(Some(&sidebar(step % 4, 3 + step % 5)));
                    full.set_sidebar(Some(&sidebar(step % 4, 3 + step % 5)));
                }
                1 => state.texts[3].push('x'),
                2 => {
                    first += 2;
                    state.set_texts(lines(first, n));
                    scrolled = 2;
                }
                3 => {
                    inc.set_overlay(Some(&overlay(1, "ab", 3)));
                    full.set_overlay(Some(&overlay(1, "ab", 3)));
                }
                4 => {
                    first -= 1;
                    state.set_texts(lines(first, n));
                    scrolled = -1;
                    inc.set_overlay(None);
                    full.set_overlay(None);
                }
                _ => {
                    if step % 12 == 5 {
                        inc.set_sidebar(None);
                        full.set_sidebar(None);
                    }
                    state.set_caret(Some((step % n, 2)));
                }
            }
            let n_now = inc.metrics().rows_for(300);
            assert_eq!(
                n_now, n,
                "la barre latérale ne change pas la hauteur des rangées"
            );
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
    fn boutons_de_la_barre_d_onglets_et_de_la_barre_d_etat() {
        let mut r = renderer();
        assert_eq!(r.tab_button_at(5.0, 5.0), None, "pas de barre");
        r.set_tabs(&[tab("a.rs", false, true), tab("b.rs", true, false)]);
        let bar = r.metrics().tab_bar as f32;
        let w = r.size().0 as f32;
        assert_eq!(r.tab_button_at(8.0, 5.0), Some(TabButton::Previous));
        assert_eq!(r.tab_button_at(30.0, 5.0), Some(TabButton::Next));
        assert_eq!(r.tab_button_at(w - 34.0, 5.0), Some(TabButton::New));
        assert_eq!(r.tab_button_at(w - 8.0, 5.0), Some(TabButton::List));
        assert_eq!(r.tab_button_at(w - 34.0, bar + 2.0), None, "sous la barre");
        // le milieu de la barre appartient aux onglets
        let geo = r.tab_geometry();
        assert_eq!(r.tab_button_at(geo[0].0 as f32 + 20.0, 5.0), None);
        // icône de la barre latérale, dans la barre d'état
        let h = r.size().1 as f32;
        assert!(r.status_sidebar_button_at(12.0, h - 5.0));
        assert!(!r.status_sidebar_button_at(300.0, h - 5.0));
        assert!(
            !r.status_sidebar_button_at(12.0, 50.0),
            "hors de la barre d'état"
        );
        // avec une barre latérale, les boutons suivent les onglets
        r.set_sidebar(Some(&sidebar(0, 3)));
        let origin = r.metrics().sidebar as f32;
        assert_eq!(
            r.tab_button_at(origin - 5.0, 5.0),
            None,
            "dans la barre latérale"
        );
        assert_eq!(
            r.tab_button_at(origin + 8.0, 5.0),
            Some(TabButton::Previous)
        );
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
        Overlay {
            rows,
            scroll: (n_items > 3).then_some((1, 3, n_items + 20)),
        }
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
        // l'élément sélectionné passe de la rangée 1 à la rangée 2 du panneau : seules ces deux
        // rangées sont redessinées
        let (rect, rows) = r.panel_geometry().unwrap();
        r.set_overlay(Some(&overlay(1, "a", 4)));
        assert_eq!(r.draw(&s, 0).unwrap().to_vec(), vec![rows[1], rows[2]]);
        // fermer le panneau repeint la zone qu'il recouvrait
        r.set_overlay(None);
        assert_eq!(r.draw(&s, 0).unwrap().to_vec(), vec![rect]);
        // aucun changement : rien à faire
        assert!(r.draw(&s, 0).unwrap().is_empty());
    }

    #[test]
    fn clic_dans_le_panneau() {
        let mut r = renderer();
        assert_eq!(r.overlay_row_at(300.0, 40.0), None, "pas de panneau");
        r.set_overlay(Some(&overlay(0, "", 3)));
        let (rect, rows) = r.panel_geometry().unwrap();
        let cx = (rect.left + rect.right) as f32 / 2.0;
        let mid = |i: usize| (rows[i].top + rows[i].bottom) as f32 / 2.0;
        assert_eq!(r.overlay_row_at(cx, mid(0)), Some(0), "saisie");
        assert_eq!(r.overlay_row_at(cx, mid(1)), Some(1));
        assert_eq!(r.overlay_row_at(cx, mid(3)), Some(3));
        // sous le panneau et à gauche du panneau
        assert_eq!(r.overlay_row_at(cx, rect.bottom as f32 + 2.0), None);
        assert_eq!(r.overlay_row_at(rect.left as f32 - 2.0, mid(1)), None);
        assert!(r.overlay_columns() >= 8);
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

    /// Décompose le coût d'une frappe (fenêtre 1100x720, minimap de 5 000 lignes) :
    /// `cargo test --release -p paulwrite-render mesure_frappe -- --ignored --nocapture`.
    #[test]
    #[ignore = "mesure"]
    fn mesure_frappe() {
        struct Big;
        impl MinimapSource for Big {
            fn len(&self) -> usize {
                5000
            }
            fn line(&self, index: usize, out: &mut String) {
                out.push_str(&format!(
                    "{}ligne {index} fn main() {{ let x = {}; }} // commentaire plus long",
                    " ".repeat(index % 3 * 4),
                    index * 7
                ));
            }
        }
        let size = (1100u32, 720u32);
        let mut r = SoftRenderer::new(1.0, size, Colors::default()).unwrap();
        let window = FakeWindow::new(size);
        r.set_tabs(&[tab("a.rs", false, true)]);
        let n = r.metrics().rows_for(size.1);
        r.set_chrome(&chrome("Line 1, Column 1", 100, n));
        r.set_minimap(&Big, 0).unwrap();
        let mut state = Rows::new(lines(100, n), Some((3, 0)), None);
        r.draw(&state, 0).unwrap();
        window.present(&mut r);
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let med = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            (v[v.len() / 2], v[v.len() * 95 / 100])
        };
        // 0 : texte seul ; 1 : + barre d'état ; 2 : + minimap reconstruite
        for mode in 0..3 {
            let mut t_draw = Vec::new();
            for step in 1..=200u64 {
                state.texts[3].push('x');
                if mode >= 1 {
                    let label = format!("Line 4, Column {mode}{step}");
                    r.set_chrome(&chrome(&label, 100, n));
                }
                if mode >= 2 {
                    r.set_minimap(&Big, 1000 * mode + step).unwrap();
                }
                let t = std::time::Instant::now();
                r.draw(&state, 0).unwrap();
                t_draw.push(ms(t));
            }
            println!(
                "frappe, mode {mode} (0 texte, 1 + état, 2 + minimap) : dessin {:?} ms (p50, p95)",
                med(&mut t_draw)
            );
        }
        let _ = (&window, &mut t_map_unused());
    }

    fn t_map_unused() {}
}
