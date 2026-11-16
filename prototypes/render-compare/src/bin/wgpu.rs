//! Prototype B : winit + wgpu + atlas de glyphes (rastérisation fontdue, 1 quad instancié par glyphe).
//!
//! Variables : `WGPU_BACKEND=dx12|vulkan` (sinon choix par défaut), `VSYNC=1`, `TRACE=1`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use render_compare::{color_runs, run, Frame, Renderer, BG, COLORS, FONT_PX};
use winit::window::Window;

const ATLAS: u32 = 1024;
const PAD_X: f32 = 8.0;
const MAX_INSTANCES: usize = 40_000;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen: [f32; 2],
    _pad: [f32; 2],
}

#[derive(Clone, Copy)]
struct Glyph {
    uv: [f32; 4],
    w: f32,
    h: f32,
    xmin: f32,
    ymin: f32,
}

struct Wgpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    instances: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    font: fontdue::Font,
    px: f32,
    glyphs: HashMap<char, Glyph>,
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
    scratch: Vec<Instance>,
    ascent: f32,
    advance: f32,
    line_h: f32,
}

const SHADER: &str = r#"
struct Globals { screen: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct Inst {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
};
struct VOut {
    @builtin(position) p: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: Inst) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0));
    let c = corners[vi];
    let px = i.pos + c * i.size;
    let ndc = vec2<f32>(px.x / g.screen.x * 2.0 - 1.0, 1.0 - px.y / g.screen.y * 2.0);
    var o: VOut;
    o.p = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = mix(i.uv.xy, i.uv.zw, c);
    o.color = i.color;
    return o;
}

@fragment
fn fs(v: VOut) -> @location(0) vec4<f32> {
    let a = textureSample(atlas, samp, v.uv).r;
    return vec4<f32>(v.color.rgb, v.color.a * a);
}
"#;

fn create(window: Arc<Window>) -> Wgpu {
    let t = Instant::now();
    let lap = |what: &str| {
        if std::env::var("TRACE").is_ok() {
            eprintln!("  [init] {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
    };
    let size = window.inner_size();
    let scale = window.scale_factor() as f32;
    let vsync = std::env::var("VSYNC").is_ok_and(|v| v == "1");

    let backends = match std::env::var("WGPU_BACKEND").as_deref() {
        Ok("dx12") => wgpu::Backends::DX12,
        Ok("vulkan") => wgpu::Backends::VULKAN,
        _ => wgpu::Backends::PRIMARY,
    };
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    let instance = wgpu::Instance::new(desc);
    lap("Instance::new");
    let surface = instance.create_surface(window.clone()).expect("surface");
    lap("create_surface");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: Some(&surface),
        apply_limit_buckets: false,
    }))
    .expect("adapter");
    lap(&format!("request_adapter ({:?})", adapter.get_info().backend));
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .expect("device");
    lap("request_device");

    let caps = surface.get_capabilities(&adapter);
    let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
    let present_mode = if vsync {
        wgpu::PresentMode::AutoVsync
    } else {
        wgpu::PresentMode::AutoNoVsync
    };
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode,
        desired_maximum_frame_latency: 1,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        color_space: wgpu::SurfaceColorSpace::Auto,
    };
    surface.configure(&device, &config);
    lap("surface.configure");

    // --- police et atlas -------------------------------------------------------------------
    let font_bytes = std::fs::read(r"C:\Windows\Fonts\consola.ttf").expect("Consolas");
    let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default()).expect("font");
    let px = FONT_PX * scale;
    let lm = font.horizontal_line_metrics(px).expect("metrics");
    let (ascent, line_h) = (lm.ascent, lm.new_line_size.ceil());
    let advance = font.metrics('M', px).advance_width;
    lap("police chargée");

    let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atlas"),
        size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let globals = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("globals"),
        size: std::mem::size_of::<Globals>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let instances = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (MAX_INSTANCES * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    lap("pipeline + shaders");

    let mut w = Wgpu {
        surface,
        device,
        queue,
        config,
        pipeline,
        bind_group,
        globals,
        instances,
        atlas_tex,
        font,
        px,
        glyphs: HashMap::new(),
        cursor_x: 2, // 2x2 texels blancs réservés en (0,0) pour les rectangles pleins
        cursor_y: 0,
        row_h: 2,
        scratch: Vec::with_capacity(8192),
        ascent,
        advance,
        line_h,
    };
    // Bloc blanc (curseur) puis ASCII imprimable.
    w.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &w.atlas_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255u8; 4],
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(2), rows_per_image: Some(2) },
        wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
    );
    for c in ' '..='~' {
        w.glyph(c);
    }
    w.update_globals();
    lap("atlas ASCII");
    w
}

impl Wgpu {
    fn update_globals(&self) {
        let g = Globals { screen: [self.config.width as f32, self.config.height as f32], _pad: [0.0; 2] };
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&g));
    }

    fn glyph(&mut self, c: char) -> Glyph {
        if let Some(g) = self.glyphs.get(&c) {
            return *g;
        }
        let (m, bitmap) = self.font.rasterize(c, self.px);
        let (gw, gh) = (m.width as u32, m.height as u32);
        if self.cursor_x + gw + 1 > ATLAS {
            self.cursor_x = 0;
            self.cursor_y += self.row_h + 1;
            self.row_h = 0;
        }
        let (x, y) = (self.cursor_x, self.cursor_y);
        self.cursor_x += gw + 1;
        self.row_h = self.row_h.max(gh);
        if gw > 0 && gh > 0 {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &bitmap,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(gw), rows_per_image: Some(gh) },
                wgpu::Extent3d { width: gw, height: gh, depth_or_array_layers: 1 },
            );
        }
        let s = ATLAS as f32;
        let g = Glyph {
            uv: [x as f32 / s, y as f32 / s, (x + gw) as f32 / s, (y + gh) as f32 / s],
            w: gw as f32,
            h: gh as f32,
            xmin: m.xmin as f32,
            ymin: m.ymin as f32,
        };
        self.glyphs.insert(c, g);
        g
    }
}

impl Renderer for Wgpu {
    fn render(&mut self, frame: &Frame<'_>, _vsync: bool) {
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        for (row, line) in frame.lines.iter().enumerate() {
            let top = row as f32 * self.line_h;
            let baseline = top + self.ascent;
            for (s, e, c) in color_runs(line.text) {
                let color = COLORS[c];
                for (k, ch) in line.text[s..e].chars().enumerate() {
                    if ch == ' ' {
                        continue;
                    }
                    let col = s + k;
                    let g = self.glyph(ch);
                    if g.w == 0.0 {
                        continue;
                    }
                    scratch.push(Instance {
                        pos: [PAD_X + col as f32 * self.advance + g.xmin, baseline - g.ymin - g.h],
                        size: [g.w, g.h],
                        uv: g.uv,
                        color,
                    });
                }
            }
        }
        if let Some((row, col)) = frame.cursor {
            scratch.push(Instance {
                pos: [PAD_X + col as f32 * self.advance, row as f32 * self.line_h],
                size: [2.0, self.line_h],
                uv: [0.25, 0.25, 0.25, 0.25],
                color: COLORS[3],
            });
        }
        scratch.truncate(MAX_INSTANCES);
        self.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&scratch));
        let count = scratch.len() as u32;
        self.scratch = scratch;

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            other => {
                eprintln!("surface: {other:?}");
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(BG[0]),
                            g: f64::from(BG[1]),
                            b: f64::from(BG[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, 0..count);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(output);
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        self.update_globals();
    }

    fn line_height(&self) -> f32 {
        self.line_h
    }
}

fn main() {
    run("wgpu + atlas de glyphes", create);
}
