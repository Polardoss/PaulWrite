//! L'interface autour du texte : barre d'onglets, gouttière, barre d'état, minimap, barre de
//! défilement et panneau flottant (palette de commandes). Tout est dessiné par `SoftRenderer`
//! dans la même DIB que le texte ; les coordonnées sont celles de la fenêtre (une translation
//! de la cible Direct2D ramène à la bande en cours).

use windows::Win32::Graphics::Direct2D::{D2D1_ELLIPSE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_RANGE};

use super::*;

// --- Types publics ---------------------------------------------------------------------------

/// Position de la fenêtre de rangées dans le document, pour la barre de défilement et la minimap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scroll {
    /// Première ligne visible.
    pub first: usize,
    /// Nombre de rangées visibles.
    pub visible: usize,
    /// Nombre de lignes du document.
    pub total: usize,
}

/// État de l'interface fourni à chaque image.
#[derive(Clone, Copy, Debug)]
pub struct Chrome<'a> {
    /// Nombre de chiffres du plus grand numéro de ligne (0 : pas de gouttière).
    pub line_digits: usize,
    /// Texte à gauche de la barre d'état (« Line 1, Column 1 »).
    pub status_left: &'a str,
    /// Textes à droite de la barre d'état, de gauche à droite (« Tab Size: 4 », « Rust »).
    pub status_right: &'a [&'a str],
    /// Position dans le document (`None` : ni minimap ni barre de défilement).
    pub scroll: Option<Scroll>,
}

/// Source de texte de la minimap (tout le document, pas seulement les rangées visibles).
pub trait MinimapSource {
    /// Nombre de lignes du document.
    fn len(&self) -> usize;
    /// Texte de la ligne `index`, sans fin de ligne, copié dans `out` (vidé par l'appelant).
    fn line(&self, index: usize, out: &mut String);
    /// Plages colorées de la ligne `index` dont le texte est `text` (celui de [`line`](Self::line)) :
    /// `(début, fin, catégorie)`, positions en **caractères**, triées et sans chevauchement,
    /// ajoutées à `out` (vidé par l'appelant). Par défaut, aucune couleur : texte gris.
    fn colors(&self, _index: usize, _text: &str, _out: &mut Vec<(usize, usize, u8)>) {}
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Boutons de la barre d'onglets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabButton {
    Previous,
    Next,
    New,
    /// Liste des onglets ouverts (flèche en haut à droite).
    List,
}

/// Nature d'une ligne de la barre latérale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarKind {
    /// Titre de la section (« FOLDERS »).
    Header,
    File,
    FolderClosed,
    FolderOpen,
}

/// Famille d'un fichier, pour l'icône de la barre latérale (une teinte par famille).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileIcon {
    #[default]
    Plain,
    Rust,
    Python,
    /// JavaScript et TypeScript.
    Script,
    /// HTML et CSS.
    Web,
    /// JSON, TOML, YAML…
    Data,
    /// Markdown et texte.
    Doc,
}

impl FileIcon {
    /// Indice du pinceau de l'icône (0 : dossier).
    fn brush(self) -> usize {
        BR_ICON
            + match self {
                FileIcon::Plain => 1,
                FileIcon::Rust => 2,
                FileIcon::Python => 3,
                FileIcon::Script => 4,
                FileIcon::Web => 5,
                FileIcon::Data => 6,
                FileIcon::Doc => 7,
            }
    }
}

/// Pinceau d'un état Git : couleurs déjà dans la palette (nombre : orange, chaîne : vert, fonction :
/// bleu, erreur : rouge), gris pour un fichier ignoré.
fn git_brush(mark: GitMark) -> Option<usize> {
    // le pinceau de la catégorie de coloration `k` est à l'indice `3 + k`
    Some(match mark {
        GitMark::None => return None,
        GitMark::Modified | GitMark::Dirty => 3 + 3,
        GitMark::Added | GitMark::Untracked => 3 + 2,
        GitMark::Renamed => 3 + 5,
        GitMark::Deleted | GitMark::Conflict => BR_SQUIGGLE,
        GitMark::Ignored => BR_SIDEBAR_DIM,
    })
}

/// Ce que Git dit d'un fichier ou d'un dossier de la barre latérale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GitMark {
    /// Rien à signaler (ou pas de dépôt).
    #[default]
    None,
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
    /// Ignoré par `.gitignore` : nom grisé.
    Ignored,
    /// Dossier qui contient au moins un changement : un point.
    Dirty,
}

impl GitMark {
    /// Lettre affichée à droite de la ligne (`None` : pas de lettre).
    #[must_use]
    pub fn letter(self) -> Option<&'static str> {
        match self {
            GitMark::Modified => Some("M"),
            GitMark::Added => Some("A"),
            GitMark::Deleted => Some("D"),
            GitMark::Renamed => Some("R"),
            GitMark::Untracked => Some("U"),
            GitMark::Conflict => Some("!"),
            GitMark::None | GitMark::Ignored | GitMark::Dirty => None,
        }
    }
}

/// Une ligne de la barre latérale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    /// État Git du fichier ou du dossier.
    pub git: GitMark,
    pub label: String,
    /// Niveau d'imbrication (0 : dossier racine).
    pub depth: usize,
    pub kind: SidebarKind,
    pub selected: bool,
    /// Icône d'un fichier (les dossiers ont la leur).
    pub icon: FileIcon,
}

/// Contenu visible de la barre latérale (les lignes déjà défilées sont retirées par l'appelant).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidebar {
    pub rows: Vec<SidebarRow>,
    /// Barre de défilement : `(première ligne affichée, lignes affichées, total)` ; `None` si tout
    /// tient.
    pub scroll: Option<(usize, usize, usize)>,
}

/// Ce que l'on touche dans la zone de droite.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StripHit {
    /// La minimap, sur la ligne du document indiquée.
    Minimap { line: usize },
    /// La barre de défilement, à cette fraction (0..1) de sa hauteur.
    Scrollbar { fraction: f32 },
}

// --- État interne ------------------------------------------------------------------------------

#[derive(Default)]
pub(super) struct Status {
    pub digits: usize,
    left: String,
    right: Vec<String>,
    pub scroll: Option<Scroll>,
    /// Les éléments de droite ont changé : toute la largeur de la barre est à redessiner (sinon
    /// seule la partie gauche, qui change à chaque frappe).
    pub right_dirty: bool,
    /// Abscisse de fin du texte de gauche tel qu'il est dessiné.
    pub left_extent: f32,
}

pub(super) struct UiLayout {
    layout: IDWriteTextLayout,
    width: f32,
    height: f32,
}

#[derive(Default)]
pub(super) struct MinimapState {
    /// Image de la minimap (pixels opaques, `width * height`) copiée dans la DIB sans passer par
    /// Direct2D (une image Direct2D de cette taille coûtait 10 fois plus à froid).
    pixels: Vec<u32>,
    width: usize,
    ready: bool,
    key: Option<(u64, usize, usize, i32, i32)>,
    /// Première ligne du document représentée en haut de la minimap.
    first: usize,
}

#[derive(Clone, Copy)]
enum UiFont {
    Tab = 0,
    Small = 1,
    Panel = 2,
}

/// Sel pour que les mises en page de l'interface n'entrent pas en collision avec celles du texte.
const UI_SALT: u64 = 0x9e37_79b9_7f4a_7c15;
const MAX_UI_LAYOUTS: usize = 512;

fn d2d_rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

impl SoftRenderer {
    // --- Texte de l'interface ----------------------------------------------------------------

    fn ui_layout(
        &mut self,
        text: &str,
        font: UiFont,
    ) -> Result<Option<(IDWriteTextLayout, f32, f32)>> {
        if text.is_empty() {
            return Ok(None);
        }
        let key = hash_bytes(text.as_bytes()) ^ UI_SALT.wrapping_mul(font as u64 + 1);
        if let Some(c) = self.ui_layouts.get(&key) {
            return Ok(Some((c.layout.clone(), c.width, c.height)));
        }
        if self.ui_layouts.len() >= MAX_UI_LAYOUTS {
            self.ui_layouts.clear();
        }
        let wide: Vec<u16> = displayed(text).encode_utf16().collect();
        // SAFETY: création d'une mise en page DirectWrite à partir de données valides.
        let (layout, m) = unsafe {
            let layout =
                self.dwrite
                    .CreateTextLayout(&wide, &self.ui[font as usize], 1.0e6, 1000.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            (layout, m)
        };
        self.ui_layouts.insert(
            key,
            UiLayout {
                layout: layout.clone(),
                width: m.width,
                height: m.height,
            },
        );
        Ok(Some((layout, m.width, m.height)))
    }

    /// Largeur d'un texte de l'interface.
    fn ui_width(&mut self, text: &str, font: UiFont) -> Result<f32> {
        Ok(self.ui_layout(text, font)?.map_or(0.0, |(_, w, _)| w))
    }

    /// Dessine un texte de l'interface centré verticalement dans `[top, bottom)` ; renvoie sa
    /// largeur.
    unsafe fn ui_text(
        &mut self,
        text: &str,
        font: UiFont,
        x: f32,
        (top, bottom): (f32, f32),
        brush: usize,
    ) -> Result<f32> {
        let Some((layout, width, height)) = self.ui_layout(text, font)? else {
            return Ok(0.0);
        };
        self.rt.DrawTextLayout(
            Vector2 {
                X: x,
                Y: top + ((bottom - top - height) / 2.0).floor(),
            },
            &layout,
            &self.brushes[brush],
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        Ok(width)
    }

    // --- État fourni par l'application -------------------------------------------------------

    /// Fournit la gouttière, la barre d'état et la position de défilement de cette image. Seules
    /// les régions dont le contenu change sont redessinées.
    pub fn set_chrome(&mut self, c: &Chrome<'_>) {
        if self.status.left != c.status_left {
            c.status_left.clone_into(&mut self.status.left);
            self.status_clean = false;
        }
        if self.status.right.len() != c.status_right.len()
            || self
                .status
                .right
                .iter()
                .zip(c.status_right)
                .any(|(a, b)| a != b)
        {
            self.status.right = c.status_right.iter().map(|s| (*s).to_owned()).collect();
            self.status.right_dirty = true;
            self.status_clean = false;
        }
        if self.status.scroll != c.scroll {
            self.status.scroll = c.scroll;
            self.strip_dirty = true;
        }
        self.status.digits = c.line_digits;
        self.refresh_metrics();
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
                git: t.git,
            })
            .collect();
        if entries != self.tabs {
            self.tabs = entries;
            self.tabs_clean = false;
        }
        self.refresh_metrics();
    }

    /// Largeur réservée aux boutons à gauche des onglets.
    fn tabs_left(&self) -> f32 {
        self.metrics.sidebar as f32 + (40.0 * self.scale).round()
    }

    /// Largeur réservée au bouton « + » à droite des onglets.
    fn tabs_right(&self) -> f32 {
        (56.0 * self.scale).round()
    }

    /// Géométrie des onglets : `(x0, x1)`. Chaque onglet a une largeur d'au moins 170 px logiques
    /// (comme Sublime Text) qui grandit avec son libellé ; si tout ne tient pas, les onglets se
    /// resserrent. Indépendante de la mesure du texte : elle sert aussi aux clics.
    pub(super) fn tab_geometry(&self) -> Vec<(i32, i32)> {
        let s = self.scale;
        let wants: Vec<f32> = self
            .tabs
            .iter()
            .map(|t| {
                let chars = t.label.chars().count() as f32;
                ((40.0 + chars * 6.6) * s).clamp(170.0 * s, 320.0 * s)
            })
            .collect();
        let avail = (self.size.0 as f32 - self.tabs_left() - self.tabs_right()).max(1.0);
        let total: f32 = wants.iter().sum();
        let k = if total > avail { avail / total } else { 1.0 };
        let mut x = self.tabs_left();
        wants
            .iter()
            .map(|w| {
                let x0 = x;
                x += (w * k).floor();
                (x0 as i32, x as i32)
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
        let close = (26.0 * self.scale).round();
        for (i, (x0, x1)) in self.tab_geometry().into_iter().enumerate() {
            if x >= x0 as f32 && x < x1 as f32 {
                return Some((i, x >= x1 as f32 - close));
            }
        }
        None
    }

    /// Bouton de la barre d'onglets sous le point `(x, y)`.
    #[must_use]
    pub fn tab_button_at(&self, x: f32, y: f32) -> Option<TabButton> {
        if self.tabs.is_empty() || y < 0.0 || y >= self.metrics.tab_bar as f32 {
            return None;
        }
        let left = self.tabs_left();
        let w = self.size.0 as f32;
        let origin = self.metrics.sidebar as f32;
        if x < origin {
            return None;
        }
        if x < left {
            return Some(if x < (origin + left) / 2.0 {
                TabButton::Previous
            } else {
                TabButton::Next
            });
        }
        let s = self.scale;
        if x >= w - 24.0 * s {
            Some(TabButton::List)
        } else if x >= w - 46.0 * s {
            Some(TabButton::New)
        } else {
            None
        }
    }

    /// Dessine la barre d'onglets (en entier ; la cible Direct2D rogne à la bande en cours).
    pub(super) unsafe fn paint_tabs(&mut self, w: i32) -> Result<()> {
        let s = self.scale;
        let bar_h = self.metrics.tab_bar as f32;
        let origin = self.metrics.sidebar as f32;
        self.rt.FillRectangle(
            &d2d_rect(origin, 0.0, w as f32, bar_h),
            &self.brushes[BR_TAB_BAR],
        );
        // Boutons de défilement des onglets (triangles) à gauche.
        let cy = (bar_h / 2.0).round();
        let t = (4.5 * s).round().max(2.0);
        for (cx, dir) in [(origin + 12.0 * s, -1.0_f32), (origin + 26.0 * s, 1.0)] {
            self.triangle(cx.round(), cy, t, dir, BR_TAB_BUTTON);
        }
        let geometry = self.tab_geometry();
        let radius = (4.0 * s).round();
        let label_pad = (11.0 * s).round();
        let close_w = (26.0 * s).round();
        for (i, (x0, x1)) in geometry.iter().copied().enumerate() {
            let tab = self.tabs[i].clone();
            let (xf0, xf1) = (x0 as f32, x1 as f32);
            if tab.active {
                self.rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: d2d_rect(xf0, (s).round(), xf1, bar_h + radius),
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &self.brushes[BR_BG],
                );
            }
            let hover = self.hover_tab.filter(|&(h, _)| h == i);
            // couleur de l'état Git (modifié, ajouté…), sinon celle de l'onglet
            let git_tint = match tab.git {
                GitMark::Ignored | GitMark::Dirty | GitMark::None => None,
                mark => git_brush(mark),
            };
            let brush = if let Some(b) = git_tint {
                b
            } else if tab.active || hover.is_some() {
                BR_TAB_TEXT_ACTIVE
            } else {
                BR_TAB_TEXT
            };
            // Libellé rogné à sa place.
            let clip = d2d_rect(xf0 + label_pad, 0.0, xf1 - close_w, bar_h);
            self.rt
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            self.ui_text(
                &tab.label,
                UiFont::Tab,
                xf0 + label_pad,
                (0.0, bar_h),
                brush,
            )?;
            self.rt.PopAxisAlignedClip();
            // Pastille (modifié) ou croix (fermer).
            let mx = xf1 - (14.0 * s).round();
            if tab.modified {
                self.rt.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: mx, Y: cy },
                        radiusX: 3.5 * s,
                        radiusY: 3.5 * s,
                    },
                    &self.brushes[BR_TAB_BUTTON],
                );
            } else {
                let a = 3.5 * s;
                let width = (1.2 * s).max(1.0);
                // la croix devient blanche quand le pointeur est dessus
                let cross = if hover == Some((i, true)) {
                    BR_TAB_TEXT_ACTIVE
                } else {
                    BR_TAB_CLOSE
                };
                for (p0, p1) in [
                    ((mx - a, cy - a), (mx + a, cy + a)),
                    ((mx - a, cy + a), (mx + a, cy - a)),
                ] {
                    self.rt.DrawLine(
                        Vector2 { X: p0.0, Y: p0.1 },
                        Vector2 { X: p1.0, Y: p1.1 },
                        &self.brushes[cross],
                        width,
                        None,
                    );
                }
            }
        }
        // Bouton « + » et flèche de la liste des onglets, à droite.
        let px = (w as f32 - 34.0 * s).round();
        let a = (4.0 * s).round().max(2.0);
        let width = (1.2 * s).max(1.0);
        for (p0, p1) in [((px - a, cy), (px + a, cy)), ((px, cy - a), (px, cy + a))] {
            self.rt.DrawLine(
                Vector2 { X: p0.0, Y: p0.1 },
                Vector2 { X: p1.0, Y: p1.1 },
                &self.brushes[BR_TAB_BUTTON],
                width,
                None,
            );
        }
        let ax = (w as f32 - 13.0 * s).round();
        let half = (4.5 * s).round().max(2.0);
        let rows = (half * 1.6) as i32;
        for r in 0..=rows {
            let len = half - r as f32 * half / (rows as f32 + 0.5);
            self.rt.FillRectangle(
                &d2d_rect(ax - len, cy - 2.0 + r as f32, ax + len, cy - 1.0 + r as f32),
                &self.brushes[BR_TAB_BUTTON],
            );
        }
        Ok(())
    }

    /// Triangle plein, pointe vers la gauche (`dir < 0`) ou la droite, centré en `(cx, cy)`.
    unsafe fn triangle(&self, cx: f32, cy: f32, half: f32, dir: f32, brush: usize) {
        // Dessin par rangées (1 px de haut) : de la base à la pointe.
        let h = (half * 2.0) as i32;
        for r in 0..=h {
            let dy = r as f32 - half;
            let len = (half - dy.abs()).max(0.0) + 1.0;
            let (l, rr) = if dir > 0.0 {
                (cx - half, cx - half + len)
            } else {
                (cx + half - len, cx + half)
            };
            self.rt.FillRectangle(
                &d2d_rect(l, cy + dy, rr, cy + dy + 1.0),
                &self.brushes[brush],
            );
        }
    }

    // --- Gouttière ---------------------------------------------------------------------------

    /// Affiche ou masque les flèches ▼ des régions repliables (la souris survole la gouttière).
    /// Seules les rangées concernées sont redessinées à l'image suivante.
    pub fn set_fold_hover(&mut self, on: bool) {
        self.fold_hover = on;
    }

    /// Rangée dont la flèche de repli est sous le pointeur (dessinée plus claire).
    pub fn set_fold_hot(&mut self, row: Option<usize>) {
        self.fold_hot_row = row;
    }

    /// Onglet sous le pointeur : `(indice, pointeur sur la croix)`. Son texte passe en blanc ;
    /// seule la barre d'onglets est redessinée.
    pub fn set_hover_tab(&mut self, hover: Option<(usize, bool)>) {
        if self.hover_tab != hover {
            self.hover_tab = hover;
            self.tabs_clean = false;
        }
    }

    /// Ligne de la barre latérale sous le pointeur : son texte passe en blanc.
    pub fn set_hover_sidebar(&mut self, row: Option<usize>) {
        if self.hover_sidebar != row {
            self.hover_sidebar = row;
            self.sidebar_clean = false;
        }
    }

    /// L'abscisse `x` est dans la zone des flèches de repli (à droite des numéros de ligne).
    #[must_use]
    pub fn gutter_fold_hit(&self, x: f32) -> bool {
        let g = self.metrics.gutter as f32;
        let right = self.metrics.sidebar as f32 + g;
        g > 0.0 && x < right && x >= right - (GUTTER_FOLD * self.scale).round()
    }

    /// Flèche de repli, dans la zone réservée à droite des numéros : ▶ si la région est repliée,
    /// ▼ si elle est repliable (au survol seulement).
    unsafe fn paint_fold_mark(&self, row: &Row<'_>, y: f32, lh: f32) {
        let show = match row.fold {
            FoldMark::None => false,
            FoldMark::Open => self.fold_hover,
            FoldMark::Closed => true,
        };
        if !show {
            return;
        }
        let g = self.metrics.gutter as f32;
        let zone = (GUTTER_FOLD * self.scale).round();
        let cx = (self.metrics.sidebar as f32 + g - zone / 2.0).round();
        let cy = (y + lh / 2.0).floor();
        let half = (4.0 * self.scale).round().max(3.0);
        let index = ((y - self.metrics.top as f32) / lh).round() as usize;
        let brush = if self.fold_hot_row == Some(index) {
            0 // pinceau du texte : la flèche sous le pointeur est presque blanche
        } else if row.current {
            BR_GUTTER_CUR_NUM
        } else {
            BR_GUTTER_NUM
        };
        if row.fold == FoldMark::Closed {
            self.triangle(cx, cy, half, 1.0, brush);
        } else {
            // pointe vers le bas : une rangée de pixels par ligne, de plus en plus étroite
            let top = cy - (half / 2.0).floor();
            for r in 0..=(half as i32) {
                let w = half - r as f32;
                self.rt.FillRectangle(
                    &d2d_rect(cx - w, top + r as f32, cx + w + 1.0, top + r as f32 + 1.0),
                    &self.brushes[brush],
                );
            }
        }
    }

    /// Numéro de ligne (aligné à droite, avant l'espace des flèches de repli).
    pub(super) unsafe fn paint_gutter(&mut self, row: &Row<'_>, y: f32, lh: f32) -> Result<()> {
        let g = self.metrics.gutter as f32;
        if g <= 0.0 {
            return Ok(());
        }
        let origin = self.metrics.sidebar as f32;
        if row.current {
            self.rt.FillRectangle(
                &d2d_rect(origin, y, origin + g, y + lh),
                &self.brushes[BR_GUTTER_CUR_BG],
            );
        }
        self.paint_fold_mark(row, y, lh);
        if row.number == 0 {
            return Ok(());
        }
        let text = row.number.to_string();
        let width = text.len() as f32 * self.metrics.advance;
        let x = origin + g - (GUTTER_FOLD * self.scale).round() - width;
        let hash = hash_bytes(text.as_bytes()) ^ UI_SALT;
        if let Some((layout, _)) = self.layout_for(&text, hash)? {
            let brush = if row.current {
                BR_GUTTER_CUR_NUM
            } else {
                BR_GUTTER_NUM
            };
            self.rt.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &layout,
                &self.brushes[brush],
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(())
    }

    /// Guides d'indentation : un trait pointillé à chaque multiple de 4 colonnes situé avant le
    /// premier caractère de la ligne.
    pub(super) unsafe fn paint_indent_guides(&self, text: &str, pad: f32, y: f32, lh: f32) {
        let mut cols = 0usize;
        for c in text.chars() {
            match c {
                ' ' => cols += 1,
                '\t' => cols = (cols / 4 + 1) * 4,
                _ => break,
            }
        }
        let step = (2.0 * self.scale).round().max(2.0);
        let mut k = 1;
        while k * 4 < cols {
            let x = (pad + (k * 4) as f32 * self.metrics.advance).round();
            let mut yy = y;
            while yy < y + lh {
                self.rt.FillRectangle(
                    &d2d_rect(x, yy, x + 1.0, (yy + 1.0).min(y + lh)),
                    &self.brushes[BR_INDENT],
                );
                yy += step;
            }
            k += 1;
        }
    }

    // --- Barre d'état ------------------------------------------------------------------------

    /// Rectangle du bouton de barre latérale (icône à gauche de la barre d'état).
    pub(super) unsafe fn paint_status(&mut self, w: i32, top: i32, bottom: i32) -> Result<()> {
        let s = self.scale;
        let (t, b) = (top as f32, bottom as f32);
        self.rt
            .FillRectangle(&d2d_rect(0.0, t, w as f32, b), &self.brushes[BR_STATUS_BG]);
        // Icône : un cadre dont le tiers gauche est plein (afficher/masquer la barre latérale).
        let (iw, ih) = ((15.0 * s).round(), (12.0 * s).round());
        let ix = (8.0 * s).round();
        let iy = (t + ((b - t - ih) / 2.0).floor()).round();
        let line = s.round().max(1.0);
        let icon = &self.brushes[BR_STATUS_ICON];
        self.rt
            .FillRectangle(&d2d_rect(ix, iy, ix + iw, iy + line), icon);
        self.rt
            .FillRectangle(&d2d_rect(ix, iy + ih - line, ix + iw, iy + ih), icon);
        self.rt
            .FillRectangle(&d2d_rect(ix, iy, ix + line, iy + ih), icon);
        self.rt
            .FillRectangle(&d2d_rect(ix + iw - line, iy, ix + iw, iy + ih), icon);
        self.rt
            .FillRectangle(&d2d_rect(ix, iy, ix + (6.0 * s).round(), iy + ih), icon);

        let left = self.status.left.clone();
        let x_left = (34.0 * s).round();
        let width = self.ui_text(&left, UiFont::Small, x_left, (t, b), BR_STATUS_TEXT)?;
        self.status.left_extent = x_left + width;
        // Éléments de droite : alignés à droite, séparés de 65 px.
        let items = self.status.right.clone();
        let mut x = w as f32 - (48.0 * s).round();
        for text in items.iter().rev() {
            let width = self.ui_width(text, UiFont::Small)?;
            x -= width;
            self.ui_text(text, UiFont::Small, x, (t, b), BR_STATUS_TEXT)?;
            x -= (65.0 * s).round();
        }
        Ok(())
    }

    /// Bande de la barre d'état à redessiner : pleine largeur si les éléments de droite ont
    /// changé, sinon la partie gauche seule (ancien et nouveau texte compris).
    pub(super) fn status_band(&mut self, top: i32, bottom: i32) -> Band {
        let w = self.size.0 as i32;
        if self.status.right_dirty {
            return Band::rows(top, bottom, w);
        }
        let left = std::mem::take(&mut self.status.left);
        let new_width = self.ui_width(&left, UiFont::Small).unwrap_or(0.0);
        self.status.left = left;
        let x = (34.0 * self.scale).round()
            + new_width.max(self.status.left_extent - (34.0 * self.scale).round());
        // à partir de l'icône exclue : elle ne change pas avec le texte
        Band {
            left: (30.0 * self.scale).round() as i32,
            right: ((x + 8.0 * self.scale).ceil() as i32).min(w),
            top,
            bottom,
        }
    }

    /// Le point est sur l'icône de la barre latérale (barre d'état, à gauche).
    #[must_use]
    pub fn status_sidebar_button_at(&self, x: f32, y: f32) -> bool {
        let h = self.size.1 as f32;
        y >= h - self.metrics.status_bar as f32 && x < (30.0 * self.scale).round()
    }

    // --- Minimap et barre de défilement ------------------------------------------------------

    fn minimap_geometry(&self) -> Option<(i32, i32, i32, i32)> {
        // (pitch, lignes visibles dans la minimap, largeur, hauteur)
        let mm_w = self.minimap_width() as i32;
        if self.status.scroll.is_none() || mm_w <= 0 {
            return None;
        }
        let s = self.scale.round().max(1.0) as i32;
        let area_h = self.size.1 as i32 - self.metrics.top - self.metrics.status_bar;
        if area_h <= 0 {
            return None;
        }
        let pitch = 2 * s;
        Some((pitch, area_h / pitch, mm_w, area_h))
    }

    /// La minimap est affichable mais son image manque (après un redimensionnement, un
    /// changement d'échelle ou de barre latérale) : à reconstruire avec [`set_minimap`](Self::set_minimap).
    #[must_use]
    pub fn minimap_needs_source(&self) -> bool {
        self.minimap_geometry().is_some() && !self.minimap.ready
    }

    /// Reconstruit l'image de la minimap si le texte ou la position ont changé. Rapide (un
    /// passage sur les lignes affichées) mais à n'appeler qu'une fois par image.
    ///
    /// # Errors
    /// Échec Direct2D (création de l'image).
    pub fn set_minimap(&mut self, src: &dyn MinimapSource, revision: u64) -> Result<()> {
        let (Some((pitch, fit, mm_w, area_h)), Some(scroll)) =
            (self.minimap_geometry(), self.status.scroll)
        else {
            self.minimap.ready = false;
            self.minimap.key = None;
            return Ok(());
        };
        let (pitch, fit) = (pitch as usize, fit as usize);
        let total = src.len();
        let first = if total <= fit {
            0
        } else {
            let max_first = (total - fit) as f64;
            let frac = scroll.first as f64 / total.saturating_sub(1).max(1) as f64;
            (frac * max_first).round() as usize
        };
        let key = (revision, first, total, mm_w, area_h);
        if self.minimap.key == Some(key) && self.minimap.ready {
            return Ok(());
        }
        let bg = argb(self.colors.background);
        let mut text = self.colors.text;
        for (t, b) in text.iter_mut().zip(self.colors.background) {
            *t = *t * 0.38 + b * 0.62;
        }
        let fg = argb(text);
        // couleur de chaque catégorie de coloration : la couleur du thème, un peu éteinte pour
        // rester discrète (comme le texte gris, mais plus présente)
        let mut palette = [fg; KIND_SLOTS];
        for (k, c) in palette.iter_mut().enumerate().skip(1) {
            *c = blend(bg, argb(self.colors.kinds[k]), 0.62);
        }
        let (w, h) = (mm_w as usize, area_h as usize);
        let mut pixels = std::mem::take(&mut self.minimap.pixels);
        pixels.clear();
        pixels.resize(w * h, bg);
        let s = self.scale.round().max(1.0) as usize;
        let mut line = String::new();
        let mut spans: Vec<(usize, usize, u8)> = Vec::new();
        for li in first..total.min(first + fit + 1) {
            line.clear();
            src.line(li, &mut line);
            spans.clear();
            src.colors(li, &line, &mut spans);
            let mut next_span = 0usize;
            let y0 = (li - first) * pitch;
            let mut col = 0usize;
            'chars: for (ci, c) in line.chars().take(600).enumerate() {
                // catégorie du caractère : les plages sont triées, on avance dans la liste
                while next_span < spans.len() && spans[next_span].1 <= ci {
                    next_span += 1;
                }
                let color = match spans.get(next_span) {
                    Some(&(a, _, k)) if a <= ci => palette[usize::from(k).min(KIND_SLOTS - 1)],
                    _ => fg,
                };
                match c {
                    '\t' => col = (col / 4 + 1) * 4,
                    c if c.is_whitespace() => col += 1,
                    _ => {
                        let x0 = col * s;
                        if x0 >= w {
                            break 'chars;
                        }
                        for dy in 0..pitch {
                            let y = y0 + dy;
                            if y >= h {
                                break;
                            }
                            for dx in 0..s {
                                if x0 + dx < w {
                                    pixels[y * w + x0 + dx] = color;
                                }
                            }
                        }
                        col += 1;
                    }
                }
            }
        }
        self.minimap.pixels = pixels;
        self.minimap.width = w;
        self.minimap.ready = true;
        self.minimap.key = Some(key);
        self.minimap.first = first;
        self.strip_dirty = true;
        Ok(())
    }

    /// Dessine la zone de droite (minimap puis barre de défilement) dans la DIB, dans la partie de
    /// `band` qu'elle recouvre, **sans passer par Direct2D** : copie de pixels, nettement moins
    /// chère (une image Direct2D de cette taille coûtait 10 fois plus à froid). À appeler après
    /// `EndDraw` ; ce que Direct2D a dessiné au même endroit (le panneau flottant) est préservé.
    pub(super) fn blit_strip(&mut self, band: &Band) {
        if self.metrics.minimap == 0 {
            return;
        }
        let (w, top) = (self.size.0 as i32, self.metrics.top);
        let bottom = self.size.1 as i32 - self.metrics.status_bar;
        let x_start = w - self.metrics.minimap;
        let (xa, xb) = (band.left.max(x_start), band.right.min(w));
        let (ya, yb) = (band.top.max(top), band.bottom.min(bottom));
        if xa >= xb || ya >= yb {
            return;
        }
        let mm_w = self.minimap_geometry().map_or(0, |g| g.2);
        let image = self.minimap.ready && self.minimap.width == mm_w as usize && mm_w > 0;
        let mm_end = x_start + if image { mm_w } else { 0 };
        let bg = argb(self.colors.background);
        let s = self.scale;
        let (tx0, tx1) = (w - (10.0 * s).round() as i32, w - (2.0 * s).round() as i32);
        let radius = (4.0 * s).round();
        let track = argb(CHROME_COLORS[BR_SCROLL_TRACK - BR_CHROME]);
        let thumb_color = argb(CHROME_COLORS[BR_SCROLL_THUMB - BR_CHROME]);
        let thumb = self.thumb(top, bottom);
        let panel = self.panel_geometry().map(|(rect, _)| rect);
        for y in ya..yb {
            let mut segments = [(xa, xb), (0, 0)];
            if let Some(p) = panel {
                if y >= p.top && y < p.bottom && p.left < xb && p.right > xa {
                    segments = [(xa, p.left.max(xa)), (p.right.min(xb), xb)];
                }
            }
            for (a, b) in segments {
                if a >= b {
                    continue;
                }
                // SAFETY: `y < hauteur` et `a..b` est dans la largeur de la DIB (`band` est
                // rognée à la fenêtre par l'appelant) ; la DIB est vivante tant que `self` l'est.
                let row = unsafe {
                    std::slice::from_raw_parts_mut(
                        self.bits.add(y as usize * w as usize + a as usize),
                        (b - a) as usize,
                    )
                };
                // minimap : copie ; le reste (colonne de la barre de défilement) : fond
                let img_end = b.min(mm_end).max(a);
                if img_end > a {
                    let src = (y - top) as usize * mm_w as usize + (a - x_start) as usize;
                    let n = (img_end - a) as usize;
                    row[..n].copy_from_slice(&self.minimap.pixels[src..src + n]);
                }
                row[(img_end - a) as usize..].fill(bg);
                // rail et curseur de la barre de défilement (bords arrondis, lissés)
                for x in a.max(tx0)..b.min(tx1) {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let slot = &mut row[(x - a) as usize];
                    let cov = rounded_coverage(
                        px,
                        py,
                        (tx0 as f32, top as f32, tx1 as f32, bottom as f32),
                        radius,
                    );
                    *slot = blend(*slot, track, cov);
                    if let Some((ty, th)) = thumb {
                        let c =
                            rounded_coverage(px, py, (tx0 as f32, ty, tx1 as f32, ty + th), radius);
                        *slot = blend(*slot, thumb_color, c);
                    }
                }
            }
        }
    }

    /// Ordonnée et hauteur du curseur de la barre de défilement. Le document peut défiler
    /// jusqu'à sa dernière ligne en haut de la fenêtre : la plage est `total - 1 + visible`.
    fn thumb(&self, top: i32, bottom: i32) -> Option<(f32, f32)> {
        let sc = self.status.scroll?;
        let track = (bottom - top) as f32;
        if track <= 0.0 {
            return None;
        }
        let range = (sc.total.saturating_sub(1) + sc.visible).max(1) as f32;
        let min = (24.0 * self.scale).round();
        let len = (track * sc.visible as f32 / range).clamp(min.min(track), track);
        let pos = (track - len) * (sc.first as f32 / sc.total.saturating_sub(1).max(1) as f32);
        Some((top as f32 + pos.round(), len.round()))
    }

    /// Minimap ou barre de défilement sous le point `(x, y)`.
    #[must_use]
    pub fn strip_hit(&self, x: f32, y: f32) -> Option<StripHit> {
        let sc = self.status.scroll?;
        let (ex1, w) = (
            self.size.0 as i32 - self.metrics.minimap,
            self.size.0 as f32,
        );
        let top = self.metrics.top as f32;
        let bottom = (self.size.1 as i32 - self.metrics.status_bar) as f32;
        if self.metrics.minimap == 0 || x < ex1 as f32 || y < top || y >= bottom {
            return None;
        }
        if x >= w - (SCROLLBAR_W * self.scale).round() {
            return Some(StripHit::Scrollbar {
                fraction: ((y - top) / (bottom - top)).clamp(0.0, 1.0),
            });
        }
        let (pitch, _, _, _) = self.minimap_geometry()?;
        let line = self.minimap.first + ((y - top) as i32 / pitch) as usize;
        Some(StripHit::Minimap {
            line: line.min(sc.total.saturating_sub(1)),
        })
    }

    // --- Barre latérale ----------------------------------------------------------------------

    /// Affiche, remplace ou retire (`None`) la barre latérale. Un changement de visibilité décale
    /// tout (redessin complet) ; un changement de contenu ne redessine que la barre.
    pub fn set_sidebar(&mut self, sidebar: Option<&Sidebar>) {
        if self.sidebar.as_ref() == sidebar {
            return;
        }
        let visibility_changed = self.sidebar.is_some() != sidebar.is_some();
        self.sidebar = sidebar.cloned();
        self.sidebar_clean = false;
        if visibility_changed {
            self.minimap = MinimapState::default();
            self.refresh_metrics();
            self.valid = false;
        }
    }

    fn sidebar_row_h(&self) -> i32 {
        (24.0 * self.scale).round() as i32
    }

    /// Nombre de lignes que la barre latérale peut afficher.
    #[must_use]
    pub fn sidebar_capacity(&self) -> usize {
        let h = self.size.1 as i32 - self.metrics.status_bar;
        (h.max(0) / self.sidebar_row_h().max(1)) as usize
    }

    /// Ligne de la barre latérale sous le point `(x, y)`.
    #[must_use]
    pub fn sidebar_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let sb = self.sidebar.as_ref()?;
        if x < 0.0 || x >= self.metrics.sidebar as f32 || y < 0.0 {
            return None;
        }
        let i = (y as i32 / self.sidebar_row_h().max(1)) as usize;
        (i < sb.rows.len()).then_some(i)
    }

    /// Dessine la barre latérale (colonne de gauche, de haut en bas jusqu'à la barre d'état).
    pub(super) unsafe fn paint_sidebar(&mut self, width: i32, bottom: i32) -> Result<()> {
        let Some(sb) = self.sidebar.clone() else {
            return Ok(());
        };
        let s = self.scale;
        let (w, b) = (width as f32, bottom as f32);
        self.rt
            .FillRectangle(&d2d_rect(0.0, 0.0, w, b), &self.brushes[BR_SIDEBAR_BG]);
        let row_h = self.sidebar_row_h() as f32;
        let indent = (14.0 * s).round();
        for (i, row) in sb.rows.iter().enumerate() {
            let (t, bt) = (i as f32 * row_h, (i + 1) as f32 * row_h);
            if t >= b {
                break;
            }
            if row.kind == SidebarKind::Header {
                self.ui_text(
                    &row.label.to_uppercase(),
                    UiFont::Small,
                    (14.0 * s).round(),
                    (t, bt),
                    BR_SIDEBAR_DIM,
                )?;
                continue;
            }
            if row.selected {
                self.rt
                    .FillRectangle(&d2d_rect(0.0, t, w, bt), &self.brushes[BR_SIDEBAR_SEL]);
            }
            let x = (12.0 * s).round() + row.depth as f32 * indent;
            let cy = ((t + bt) / 2.0).round();
            // sous le pointeur : texte et flèche en blanc au lieu de gris
            let hot = self.hover_sidebar == Some(i);
            let dim = if hot {
                BR_SIDEBAR_TEXT_SEL
            } else {
                BR_SIDEBAR_DIM
            };
            match row.kind {
                SidebarKind::FolderClosed => {
                    self.triangle(x + 4.0 * s, cy, (3.5 * s).round(), 1.0, dim);
                }
                SidebarKind::FolderOpen => {
                    let half = (3.5 * s).round();
                    for r in 0..=(half as i32) {
                        let len = half - r as f32;
                        self.rt.FillRectangle(
                            &d2d_rect(
                                x + 4.0 * s - len,
                                cy - half / 2.0 + r as f32,
                                x + 4.0 * s + len + 1.0,
                                cy - half / 2.0 + r as f32 + 1.0,
                            ),
                            &self.brushes[dim],
                        );
                    }
                }
                _ => {}
            }
            // icône (dossier ou famille du fichier), puis le nom
            let icon_x = x + (14.0 * s).round();
            let icon_bg = if row.selected {
                BR_SIDEBAR_SEL
            } else {
                BR_SIDEBAR_BG
            };
            self.paint_icon(row, icon_x, cy, icon_bg);
            // couleur du nom selon Git (sauf ligne choisie ou survolée : blanc)
            let tint = git_brush(row.git);
            let brush = if row.selected || hot {
                BR_SIDEBAR_TEXT_SEL
            } else if let (Some(b), false) = (tint, row.git == GitMark::Dirty) {
                b
            } else {
                BR_SIDEBAR_TEXT
            };
            // lettre (fichier) ou point (dossier) à droite, dans la couleur de l'état
            let marked = row.git.letter().is_some() || row.git == GitMark::Dirty;
            if let (true, Some(b)) = (marked, tint) {
                if let Some(letter) = row.git.letter() {
                    self.ui_text(letter, UiFont::Small, w - (22.0 * s).round(), (t, bt), b)?;
                } else {
                    let r = (3.0 * s).round();
                    self.rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: d2d_rect(w - 20.0 * s - r, cy - r, w - 20.0 * s + r, cy + r),
                            radiusX: r,
                            radiusY: r,
                        },
                        &self.brushes[b],
                    );
                }
            }
            let right = if marked { 26.0 * s } else { 10.0 * s };
            let clip = d2d_rect(0.0, t, w - right, bt);
            self.rt
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            self.ui_text(
                &row.label,
                UiFont::Tab,
                icon_x + (18.0 * s).round(),
                (t, bt),
                brush,
            )?;
            self.rt.PopAxisAlignedClip();
        }
        // barre de défilement : un curseur arrondi à droite, seulement si tout ne tient pas
        if let Some((first, visible, total)) = sb.scroll {
            let (y0, th) = self.sidebar_thumb(first, visible, total, b);
            self.rt.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: d2d_rect(w - 8.0 * s, y0, w - 2.0 * s, y0 + th),
                    radiusX: 3.0 * s,
                    radiusY: 3.0 * s,
                },
                &self.brushes[BR_SCROLL_THUMB],
            );
        }
        Ok(())
    }

    /// Ordonnée et hauteur du curseur de la barre de défilement de la barre latérale.
    fn sidebar_thumb(&self, first: usize, visible: usize, total: usize, height: f32) -> (f32, f32) {
        let total = total.max(1);
        let th = (height * visible as f32 / total as f32)
            .max(28.0 * self.scale)
            .min(height);
        let range = total.saturating_sub(visible).max(1);
        let y = (height - th) * first.min(range) as f32 / range as f32;
        (y, th)
    }

    /// Le point est sur la barre de défilement de la barre latérale (bande de droite) : renvoie la
    /// fraction (0..1) du document que la souris désigne, le curseur étant centré sur elle.
    #[must_use]
    pub fn sidebar_scroll_hit(&self, x: f32, y: f32) -> Option<f32> {
        let (_, visible, total) = self.sidebar.as_ref()?.scroll?;
        let edge = self.metrics.sidebar as f32;
        let height = (self.size.1 as i32 - self.metrics.status_bar) as f32;
        if x < edge - 12.0 * self.scale || x >= edge || y < 0.0 || y >= height {
            return None;
        }
        let (_, th) = self.sidebar_thumb(0, visible, total, height);
        Some(((y - th / 2.0) / (height - th).max(1.0)).clamp(0.0, 1.0))
    }

    /// Icône d'une ligne : dossier (languette et corps) ou page à coin replié, teintée selon la
    /// famille du fichier. Formes simples remplies : pas d'image.
    unsafe fn paint_icon(&self, row: &SidebarRow, x: f32, cy: f32, bg: usize) {
        let s = self.scale;
        let px = |v: f32| (v * s).round();
        let y0 = (cy - px(6.0)).round();
        let rect = |l: f32, t: f32, r: f32, b: f32, brush: usize| {
            self.rt
                .FillRectangle(&d2d_rect(l, t, r, b), &self.brushes[brush]);
        };
        match row.kind {
            SidebarKind::FolderClosed | SidebarKind::FolderOpen => {
                // languette puis corps du dossier
                rect(x, y0 + px(1.0), x + px(5.0), y0 + px(3.0), BR_ICON);
                rect(x, y0 + px(3.0), x + px(13.0), y0 + px(11.0), BR_ICON);
            }
            SidebarKind::File => {
                let brush = row.icon.brush();
                let (l, r) = (x + px(1.5), x + px(11.5));
                rect(l, y0, r, y0 + px(12.0), brush);
                // coin replié : un triangle de la couleur du fond, en haut à droite
                let c = px(3.0).max(2.0);
                for k in 0..(c as i32) {
                    let k = k as f32;
                    rect(r - c + k, y0 + k, r, y0 + k + 1.0, bg);
                }
                // deux traits de texte sur la page
                rect(l + px(2.0), y0 + px(6.0), r - px(2.0), y0 + px(7.0), bg);
                rect(l + px(2.0), y0 + px(8.5), r - px(4.0), y0 + px(9.5), bg);
            }
            SidebarKind::Header => {}
        }
    }

    // --- Panneau flottant --------------------------------------------------------------------

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

    /// `(largeur, marge, hauteur de la saisie, hauteur d'une rangée)` du panneau.
    fn panel_metrics(&self) -> (f32, f32, f32, f32) {
        let s = self.scale;
        let w = self.size.0 as f32;
        (
            (600.0 * s).min(w - 20.0 * s).max(60.0 * s).round(),
            (4.0 * s).round(),
            (31.0 * s).round(),
            (26.0 * s).round(),
        )
    }

    /// Rectangle du panneau et rectangle de chacune de ses rangées (0 : saisie).
    pub(super) fn panel_geometry(&self) -> Option<(Band, Vec<Band>)> {
        let o = self.overlay.as_ref()?;
        let (pw, m, input_h, row_h) = self.panel_metrics();
        let x0 = ((self.size.0 as f32 - pw) / 2.0).floor();
        let (l, r) = (x0 as i32, (x0 + pw) as i32);
        let mut rows = Vec::with_capacity(o.rows.len());
        let mut y = m;
        for (i, _) in o.rows.iter().enumerate() {
            let h = if i == 0 { input_h } else { row_h };
            rows.push(Band {
                left: l + m as i32,
                right: r - m as i32,
                top: y as i32,
                bottom: (y + h) as i32,
            });
            y += h;
            if i == 0 {
                y += m;
            }
        }
        let rect = Band {
            left: l,
            right: r,
            top: 0,
            bottom: (y + m) as i32,
        };
        Some((rect, rows))
    }

    /// Bandes à redessiner pour passer du panneau dessiné au panneau courant.
    pub(super) fn overlay_bands(&mut self, now: Option<&(Band, Vec<Band>)>) {
        match (self.drawn_overlay.as_ref(), now) {
            (None, None) => {}
            (Some((old, _)), None) => self.bands.push(*old),
            (None, Some((rect, _))) => self.bands.push(*rect),
            (Some((old, old_hashes)), Some((rect, rows))) => {
                if old != rect || old_hashes.len() != self.overlay_hashes.len() {
                    self.bands.push(*old);
                    self.bands.push(*rect);
                } else {
                    for (i, band) in rows.iter().enumerate() {
                        if old_hashes[i] != self.overlay_hashes[i] {
                            self.bands.push(*band);
                        }
                    }
                }
            }
        }
    }

    /// Nombre approximatif de caractères qui tiennent sur une rangée du panneau.
    #[must_use]
    pub fn overlay_columns(&self) -> usize {
        let (pw, ..) = self.panel_metrics();
        let inner = pw - 2.0 * (22.0 * self.scale);
        (inner / (6.2 * self.scale)).floor().max(8.0) as usize
    }

    /// Rangée du panneau située sous le point `(x, y)` (pixels de la zone cliente), ou `None`
    /// si le point est hors du panneau (ou s'il n'y en a pas). 0 : la ligne de saisie.
    #[must_use]
    pub fn overlay_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let (_, rows) = self.panel_geometry()?;
        rows.iter().position(|b| {
            x >= b.left as f32 && x < b.right as f32 && y >= b.top as f32 && y < b.bottom as f32
        })
    }

    /// Dessine les rangées du panneau qui recouvrent la bande `band`.
    pub(super) unsafe fn paint_overlay(&mut self, band: &Band) -> Result<()> {
        let Some((rect, rows)) = self.panel_geometry() else {
            return Ok(());
        };
        if !rect.intersects(band) {
            return Ok(());
        }
        let Some(overlay) = self.overlay.clone() else {
            return Ok(());
        };
        let s = self.scale;
        let (px0, px1) = (rect.left as f32, rect.right as f32);
        // Fond du panneau (marges comprises) ; la saisie et les rangées se dessinent par-dessus.
        self.rt.FillRectangle(
            &d2d_rect(px0, rect.top as f32, px1, rect.bottom as f32),
            &self.brushes[BR_PANEL_BG],
        );
        let scrolled = overlay
            .scroll
            .filter(|&(_, visible, total)| total > visible && rows.len() > 1);
        for (i, (row, geo)) in overlay.rows.iter().zip(&rows).enumerate() {
            if !geo.intersects(band) {
                continue;
            }
            let (x0, t, b) = (geo.left as f32, geo.top as f32, geo.bottom as f32);
            // Une barre de défilement réduit la largeur des rangées de la liste.
            let x1 = if i > 0 && scrolled.is_some() {
                geo.right as f32 - (11.0 * s).round()
            } else {
                geo.right as f32
            };
            let clip = d2d_rect(x0, t, x1, b);
            self.rt
                .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            if i == 0 {
                self.rt
                    .FillRectangle(&d2d_rect(x0, t, x1, b), &self.brushes[BR_PANEL_INPUT]);
                let tx = x0 + (6.0 * s).round();
                let caret_x = self.panel_text(&row.text, tx, (t, b), BR_PANEL_TEXT, &[])?;
                if let Some(c) = row.caret {
                    let cx = tx + self.panel_caret_offset(&row.text, c)?;
                    let _ = caret_x;
                    self.rt.FillRectangle(
                        &d2d_rect(
                            cx,
                            t + (6.0 * s).round(),
                            cx + s.round().max(1.0),
                            b - (6.0 * s).round(),
                        ),
                        &self.brushes[BR_PANEL_TEXT],
                    );
                }
            } else {
                if row.kind == OverlayRowKind::Selected {
                    self.rt.FillRectangle(
                        &d2d_rect(x0, t + s.round(), x1, b - s.round()),
                        &self.brushes[BR_PANEL_SEL],
                    );
                }
                let tx = x0 + (6.0 * s).round();
                let brush = match row.kind {
                    OverlayRowKind::Note => BR_PANEL_NOTE,
                    OverlayRowKind::Selected => BR_PANEL_SEL_TEXT,
                    _ => BR_PANEL_TEXT,
                };
                self.panel_text(&row.text, tx, (t, b), brush, &row.highlights)?;
                if !row.hint.is_empty() {
                    let width = self.ui_width(&row.hint, UiFont::Small)?;
                    let hx = x1 - (10.0 * s).round() - width;
                    self.ui_text(&row.hint, UiFont::Small, hx, (t, b), BR_PANEL_HINT)?;
                }
            }
            self.rt.PopAxisAlignedClip();
        }
        // Barre de défilement de la liste.
        if let Some((first, visible, total)) = scrolled {
            let list_top = rows[1].top as f32;
            let list_bottom = rows[rows.len() - 1].bottom as f32;
            let track = list_bottom - list_top;
            let len = (track * visible as f32 / total as f32)
                .max(16.0 * s)
                .min(track);
            let pos = (track - len) * first as f32 / (total - visible).max(1) as f32;
            let right = px1 - (4.0 * s).round();
            self.rt.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: d2d_rect(
                        right - (7.0 * s).round(),
                        list_top + pos.round(),
                        right,
                        list_top + pos.round() + len.round(),
                    ),
                    radiusX: 3.0 * s,
                    radiusY: 3.0 * s,
                },
                &self.brushes[BR_PANEL_THUMB],
            );
        }
        Ok(())
    }

    /// Texte du panneau ; les plages `highlights` (UTF-16) sont soulignées et en gras. Renvoie
    /// la largeur du texte.
    unsafe fn panel_text(
        &mut self,
        text: &str,
        x: f32,
        (top, bottom): (f32, f32),
        brush: usize,
        highlights: &[(usize, usize)],
    ) -> Result<f32> {
        if highlights.is_empty() {
            return self.ui_text(text, UiFont::Panel, x, (top, bottom), brush);
        }
        // Mise en page propre à cette rangée (les attributs ne doivent pas fuir dans le cache).
        let wide: Vec<u16> = displayed(text).encode_utf16().collect();
        let layout =
            self.dwrite
                .CreateTextLayout(&wide, &self.ui[UiFont::Panel as usize], 1.0e6, 1000.0)?;
        for &(a, b) in highlights {
            let (a, b) = (a.min(wide.len()), b.min(wide.len()));
            if a < b {
                let range = DWRITE_TEXT_RANGE {
                    startPosition: a as u32,
                    length: (b - a) as u32,
                };
                layout.SetUnderline(true, range)?;
                layout.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, range)?;
            }
        }
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        self.rt.DrawTextLayout(
            Vector2 {
                X: x,
                Y: top + ((bottom - top - m.height) / 2.0).floor(),
            },
            &layout,
            &self.brushes[brush],
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        Ok(m.width)
    }

    /// Décalage horizontal de la position UTF-16 `caret` dans un texte du panneau.
    unsafe fn panel_caret_offset(&mut self, text: &str, caret: usize) -> Result<f32> {
        let Some((layout, units)) = self.ui_layout(text, UiFont::Panel)?.map(|(l, _, _)| {
            let n = displayed(text).encode_utf16().count();
            (l, n)
        }) else {
            return Ok(0.0);
        };
        let (mut x, mut y) = (0.0_f32, 0.0_f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        let _ = layout.HitTestTextPosition(caret.min(units) as u32, false, &mut x, &mut y, &mut m);
        Ok(x)
    }
}

/// Pixel opaque `0xAARRGGBB` d'une couleur.
fn argb(c: [f32; 3]) -> u32 {
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    0xFF00_0000 | (to(c[0]) << 16) | (to(c[1]) << 8) | to(c[2])
}

/// Couverture (0..1) du pixel de centre `(px, py)` par un rectangle `(gauche, haut, droite, bas)`
/// aux coins arrondis de rayon `radius`.
fn rounded_coverage(px: f32, py: f32, (l, t, r, b): (f32, f32, f32, f32), radius: f32) -> f32 {
    if px < l || px > r || py < t || py > b {
        return 0.0;
    }
    let cx = px.clamp(l + radius, r - radius);
    let cy = py.clamp(t + radius, b - radius);
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    (radius - d + 0.5).clamp(0.0, 1.0)
}

/// Mélange `from` vers `to` (pixels `0xAARRGGBB` opaques) selon `coverage`.
fn blend(from: u32, to: u32, coverage: f32) -> u32 {
    if coverage <= 0.0 {
        return from;
    }
    let ch = |shift: u32| {
        let (a, b) = (
            ((from >> shift) & 0xff) as f32,
            ((to >> shift) & 0xff) as f32,
        );
        (a + (b - a) * coverage).round() as u32
    };
    0xFF00_0000 | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}
