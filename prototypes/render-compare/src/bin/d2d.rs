//! Prototype A : winit + Direct3D11/DXGI (swap chain « flip ») + Direct2D + DirectWrite.

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::Arc;

use render_compare::{color_runs, run, Frame, Renderer, BG, COLORS, FONT_PX};
use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows_numerics::Vector2;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_REGULAR, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGIFactory5, IDXGISurface, IDXGISwapChain1,
    DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_PRESENT, DXGI_PRESENT_ALLOW_TEARING,
    DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

const PAD_X: f32 = 8.0;

struct D2d {
    swap: IDXGISwapChain1,
    ctx: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    brushes: Vec<ID2D1Brush>,
    layouts: HashMap<usize, (u32, IDWriteTextLayout)>,
    target: Option<ID2D1Bitmap1>,
    tearing: bool,
    advance: f32,
    line_h: f32,
}

fn create(window: &Window) -> Result<D2d> {
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
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let d3d = device.expect("device D3D11");
        lap("D3D11CreateDevice");
        let dxgi_device: IDXGIDevice = d3d.cast()?;
        let adapter = dxgi_device.GetAdapter()?;
        let factory: IDXGIFactory2 = adapter.GetParent()?;

        let mut allow: i32 = 0;
        let tearing = factory
            .cast::<IDXGIFactory5>()
            .and_then(|f5| {
                f5.CheckFeatureSupport(
                    DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                    (&mut allow as *mut i32).cast(),
                    4,
                )
            })
            .is_ok()
            && allow != 0;

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: size.width,
            Height: size.height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
        };
        lap("DXGI factory + tearing check");
        let swap = factory.CreateSwapChainForHwnd(&d3d, hwnd, &desc, None, None)?;
        lap("CreateSwapChainForHwnd");

        let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        lap("D2D1CreateFactory");
        let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
        let ctx = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        lap("D2D device + context");

        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        lap("DWriteCreateFactory");
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
        lap("text format + metrics (Consolas)");

        let mut brushes = Vec::new();
        for c in COLORS {
            let color = D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: c[3] };
            brushes.push(ctx.CreateSolidColorBrush(&color, None)?.cast::<ID2D1Brush>()?);
        }

        let mut d = D2d {
            swap,
            ctx,
            dwrite,
            format,
            brushes,
            layouts: HashMap::new(),
            target: None,
            tearing,
            advance,
            line_h,
        };
        d.bind_target()?;
        lap("brushes + target");
        Ok(d)
    }
}

impl D2d {
    unsafe fn bind_target(&mut self) -> Result<()> {
        let surface: IDXGISurface = self.swap.GetBuffer(0)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        let bitmap = self.ctx.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
        self.ctx.SetTarget(&bitmap);
        self.target = Some(bitmap);
        Ok(())
    }

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

    unsafe fn draw(&mut self, frame: &Frame<'_>, vsync: bool) -> Result<()> {
        // Purge des mises en page des lignes qui ne sont plus visibles.
        if self.layouts.len() > frame.lines.len() * 3 {
            let (lo, hi) = (
                frame.lines.first().map_or(0, |l| l.index),
                frame.lines.last().map_or(0, |l| l.index),
            );
            self.layouts.retain(|i, _| *i >= lo && *i <= hi);
        }
        self.ctx.BeginDraw();
        let bg = D2D1_COLOR_F { r: BG[0], g: BG[1], b: BG[2], a: BG[3] };
        self.ctx.Clear(Some(&bg));
        for (row, line) in frame.lines.iter().enumerate() {
            let layout = self.layout_for(line.index, line.rev, line.text)?;
            self.ctx.DrawTextLayout(
                Vector2 { X: PAD_X, Y: row as f32 * self.line_h },
                &layout,
                &self.brushes[0],
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        if let Some((row, col)) = frame.cursor {
            let x = PAD_X + col as f32 * self.advance;
            let y = row as f32 * self.line_h;
            self.ctx.FillRectangle(
                &D2D_RECT_F { left: x, top: y, right: x + 2.0, bottom: y + self.line_h },
                &self.brushes[3],
            );
        }
        self.ctx.EndDraw(None, None)?;
        let (interval, flags) = if vsync {
            (1, DXGI_PRESENT(0))
        } else if self.tearing {
            (0, DXGI_PRESENT_ALLOW_TEARING)
        } else {
            (0, DXGI_PRESENT(0))
        };
        self.swap.Present(interval, flags).ok()
    }
}

impl Renderer for D2d {
    fn render(&mut self, frame: &Frame<'_>, vsync: bool) {
        unsafe { self.draw(frame, vsync).expect("rendu D2D") }
    }

    fn resize(&mut self, width: u32, height: u32) {
        unsafe {
            self.ctx.SetTarget(None);
            self.target = None;
            let flags = if self.tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING } else { DXGI_SWAP_CHAIN_FLAG(0) };
            self.swap
                .ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, flags)
                .expect("ResizeBuffers");
            self.bind_target().expect("rebind");
        }
    }

    fn line_height(&self) -> f32 {
        self.line_h
    }
}

fn main() {
    run("Direct2D + DirectWrite", |window: Arc<Window>| create(&window).expect("init D2D"));
}
