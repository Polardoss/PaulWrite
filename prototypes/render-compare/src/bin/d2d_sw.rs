//! Prototype A2 : winit + Direct2D *logiciel* (ID2D1DCRenderTarget) + DirectWrite, sans GPU.
//!
//! Rendu dans une DIB mémoire persistante, puis copie (BitBlt) vers la fenêtre : aucun pilote
//! graphique à initialiser.

use std::collections::HashMap;
use std::sync::Arc;

use render_compare::{color_runs, run, Frame, Renderer, BG, COLORS, FONT_PX};
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
    DWRITE_FONT_WEIGHT_REGULAR, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows_numerics::Vector2;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

const PAD_X: f32 = 8.0;

struct Sw {
    hwnd: HWND,
    window_dc: HDC,
    mem_dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    size: (u32, u32),
    rt: ID2D1DCRenderTarget,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    brushes: Vec<ID2D1Brush>,
    layouts: HashMap<usize, (u32, IDWriteTextLayout)>,
    advance: f32,
    line_h: f32,
    /// Signature (ligne, révision, colonne du curseur si dans la ligne) de chaque rangée dessinée
    /// dans la DIB : permet de ne redessiner que les rangées qui changent (`DIRTY=1`).
    prev: Vec<Sig>,
    cur: Vec<Sig>,
    dirty: Vec<bool>,
    runs: Vec<(i32, i32)>,
    valid: bool,
    use_dirty: bool,
}

type Sig = (usize, u32, Option<usize>);
const NO_SIG: Sig = (usize::MAX, 0, None);

fn create(window: &Window) -> Result<Sw> {
    let t = std::time::Instant::now();
    let lap = |what: &str| {
        if std::env::var("TRACE").is_ok() {
            eprintln!("  [init] {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
    };
    let size = window.inner_size();
    let scale = window.scale_factor() as f32;
    let hwnd = match window.window_handle().expect("handle").as_raw() {
        RawWindowHandle::Win32(h) => HWND(h.hwnd.get() as *mut _),
        _ => unreachable!("Windows uniquement"),
    };
    unsafe {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        lap("D2D1CreateFactory");
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
        lap("DC render target (software)");

        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
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
        let (advance, line_h) = (m.width, m.height.ceil());
        lap("DirectWrite factory + format + metrics");

        let mut brushes = Vec::new();
        for c in COLORS {
            let color = D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: c[3] };
            brushes.push(rt.CreateSolidColorBrush(&color, None)?.cast::<ID2D1Brush>()?);
        }
        // Index 4 : couleur de fond (pour effacer une rangée sans `Clear`, qui efface tout).
        let bg = D2D1_COLOR_F { r: BG[0], g: BG[1], b: BG[2], a: BG[3] };
        brushes.push(rt.CreateSolidColorBrush(&bg, None)?.cast::<ID2D1Brush>()?);

        let window_dc = GetDC(Some(hwnd));
        let mem_dc = CreateCompatibleDC(Some(window_dc));
        let (dib, old) = make_dib(mem_dc, size.width, size.height);
        lap("GDI back buffer");
        Ok(Sw {
            hwnd,
            window_dc,
            mem_dc,
            dib,
            old,
            size: (size.width, size.height),
            rt,
            dwrite,
            format,
            brushes,
            layouts: HashMap::new(),
            advance,
            line_h,
            prev: Vec::new(),
            cur: Vec::new(),
            dirty: Vec::new(),
            runs: Vec::new(),
            valid: false,
            use_dirty: std::env::var("DIRTY").is_ok_and(|v| v == "1"),
        })
    }
}

unsafe fn make_dib(mem_dc: HDC, w: u32, h: u32) -> (HBITMAP, HGDIOBJ) {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        .expect("CreateDIBSection");
    let old = SelectObject(mem_dc, dib.into());
    (dib, old)
}

impl Sw {
    unsafe fn layout_for(&mut self, index: usize, rev: u32, text: &str) -> Result<IDWriteTextLayout> {
        if let Some((r, l)) = self.layouts.get(&index) {
            if *r == rev {
                return Ok(l.clone());
            }
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = self.dwrite.CreateTextLayout(&wide, &self.format, 8192.0, self.line_h * 2.0)?;
        for (s, e, c) in color_runs(text) {
            if e > s {
                let effect: windows::core::IUnknown = self.brushes[c].cast()?;
                layout.SetDrawingEffect(
                    &effect,
                    DWRITE_TEXT_RANGE { startPosition: s as u32, length: (e - s) as u32 },
                )?;
            }
        }
        self.layouts.insert(index, (rev, layout.clone()));
        Ok(layout)
    }

    /// Rendu par régions : détecte un défilement (copie des pixels dans la DIB), ne redessine que
    /// les rangées dont la signature change, et ne copie vers la fenêtre que ces rangées.
    unsafe fn draw_dirty(&mut self, frame: &Frame<'_>) -> Result<()> {
        if self.layouts.len() > frame.lines.len() * 3 {
            let (lo, hi) = (
                frame.lines.first().map_or(0, |l| l.index),
                frame.lines.last().map_or(0, |l| l.index),
            );
            self.layouts.retain(|i, _| *i >= lo && *i <= hi);
        }
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let lh = self.line_h as i32;
        let rows = frame.lines.len();

        self.cur.clear();
        for (r, l) in frame.lines.iter().enumerate() {
            let c = match frame.cursor {
                Some((cr, cc)) if cr == r => Some(cc),
                _ => None,
            };
            self.cur.push((l.index, l.rev, c));
        }

        // Défilement : on déplace les pixels déjà dessinés et on décale les signatures.
        if self.valid && !self.prev.is_empty() && rows > 0 {
            let delta = self.cur[0].0 as isize - self.prev[0].0 as isize;
            let d = delta.unsigned_abs();
            if d != 0 && d < rows.min(self.prev.len()) {
                // La dernière rangée peut n'être dessinée qu'en partie (coupée par le bas de la
                // DIB) : on ne la réutilise pas.
                if let Some(last) = self.prev.last_mut() {
                    *last = NO_SIG;
                }
                let dy = d as i32 * lh;
                if delta > 0 {
                    BitBlt(self.mem_dc, 0, 0, w, h - dy, Some(self.mem_dc), 0, dy, SRCCOPY)?;
                    self.prev.drain(0..d);
                } else {
                    BitBlt(self.mem_dc, 0, dy, w, h - dy, Some(self.mem_dc), 0, 0, SRCCOPY)?;
                    self.prev.splice(0..0, std::iter::repeat(NO_SIG).take(d));
                }
            }
        }

        self.dirty.clear();
        for r in 0..rows {
            let clean = self.valid && self.prev.get(r) == Some(&self.cur[r]);
            self.dirty.push(!clean);
        }

        // Rangées sales contiguës -> bandes (haut, bas) en pixels.
        self.runs.clear();
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
            let top = start as i32 * lh;
            let bottom = (r as i32 * lh).min(h);
            if top < bottom {
                self.runs.push((top, bottom));
            }
        }

        if !self.runs.is_empty() {
            let runs = std::mem::take(&mut self.runs);
            for &(top, bottom) in &runs {
                // On ne lie à la cible Direct2D que la bande modifiée : `BindDC` + `EndDraw`
                // copient toute la zone liée, donc lier l'écran entier coûte ~ la taille de l'écran.
                // Les coordonnées de dessin sont alors relatives au haut de la bande.
                let rect = RECT { left: 0, top, right: w, bottom };
                self.rt.BindDC(self.mem_dc, &rect)?;
                self.rt.BeginDraw();
                let band_h = (bottom - top) as f32;
                let band = D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: band_h };
                self.rt.FillRectangle(&band, &self.brushes[4]);
                let (r0, r1) = ((top / lh) as usize, ((bottom + lh - 1) / lh) as usize);
                for row in r0..r1.min(rows) {
                    let line = &frame.lines[row];
                    let layout = self.layout_for(line.index, line.rev, line.text)?;
                    self.rt.DrawTextLayout(
                        Vector2 { X: PAD_X, Y: (row as i32 * lh - top) as f32 },
                        &layout,
                        &self.brushes[0],
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
                if let Some((row, col)) = frame.cursor {
                    let y = (row as i32 * lh - top) as f32;
                    if y >= 0.0 && y < band_h {
                        let x = PAD_X + col as f32 * self.advance;
                        self.rt.FillRectangle(
                            &D2D_RECT_F { left: x, top: y, right: x + 2.0, bottom: y + self.line_h },
                            &self.brushes[3],
                        );
                    }
                }
                self.rt.EndDraw(None, None)?;
                BitBlt(self.window_dc, 0, top, w, bottom - top, Some(self.mem_dc), 0, top, SRCCOPY)?;
            }
            self.runs = runs;
        }

        std::mem::swap(&mut self.prev, &mut self.cur);
        self.valid = true;
        Ok(())
    }

    unsafe fn draw(&mut self, frame: &Frame<'_>) -> Result<()> {
        if self.use_dirty {
            return self.draw_dirty(frame);
        }
        if self.layouts.len() > frame.lines.len() * 3 {
            let (lo, hi) = (
                frame.lines.first().map_or(0, |l| l.index),
                frame.lines.last().map_or(0, |l| l.index),
            );
            self.layouts.retain(|i, _| *i >= lo && *i <= hi);
        }
        let (w, h) = self.size;
        let rect = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        self.rt.BindDC(self.mem_dc, &rect)?;
        self.rt.BeginDraw();
        let bg = D2D1_COLOR_F { r: BG[0], g: BG[1], b: BG[2], a: BG[3] };
        self.rt.Clear(Some(&bg));
        for (row, line) in frame.lines.iter().enumerate() {
            let layout = self.layout_for(line.index, line.rev, line.text)?;
            self.rt.DrawTextLayout(
                Vector2 { X: PAD_X, Y: row as f32 * self.line_h },
                &layout,
                &self.brushes[0],
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        if let Some((row, col)) = frame.cursor {
            let x = PAD_X + col as f32 * self.advance;
            let y = row as f32 * self.line_h;
            self.rt.FillRectangle(
                &D2D_RECT_F { left: x, top: y, right: x + 2.0, bottom: y + self.line_h },
                &self.brushes[3],
            );
        }
        self.rt.EndDraw(None, None)?;
        BitBlt(self.window_dc, 0, 0, w as i32, h as i32, Some(self.mem_dc), 0, 0, SRCCOPY)?;
        Ok(())
    }
}

impl Renderer for Sw {
    fn render(&mut self, frame: &Frame<'_>, _vsync: bool) {
        unsafe { self.draw(frame).expect("rendu D2D logiciel") }
    }

    fn resize(&mut self, width: u32, height: u32) {
        unsafe {
            SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let (dib, old) = make_dib(self.mem_dc, width, height);
            self.dib = dib;
            self.old = old;
            self.size = (width, height);
            self.valid = false;
        }
    }

    fn line_height(&self) -> f32 {
        self.line_h
    }
}

impl Drop for Sw {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.dib.into());
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(Some(self.hwnd), self.window_dc);
        }
    }
}

fn main() {
    run("Direct2D logiciel (sans GPU)", |window: Arc<Window>| create(&window).expect("init D2D logiciel"));
}
