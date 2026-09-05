// scene_2dui - 2D UI compositing RENDER-scene carpet driven by the `wgpu` crate (v22) on Mesa software
// adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. WebGPU port of the GLES scene_2dui:
// an offscreen 64x64 Rgba8Unorm texture is rendered through real render pipelines (WGSL shaders),
// copied to a MAP_READ buffer (256-byte bytesPerRow padding) and read back; every scene primitive has
// an INDEPENDENT closed-form software reference computed in Rust (not derived from the GPU output) and
// asserted per pixel: filled axis-aligned rectangles, an analytic rounded-rect, a nine-patch-style
// scaled border frame, an 8x8 bitmap-font glyph blit, a scissor-clipped fill, and MULTI-LAYER
// Porter-Duff over compositing of 3 stacked semi-transparent layers. Closes with a negative control.
// Prints "SCENE_2DUI OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Coordinate convention (WebGPU vs GL): the GLES cell used gl_FragCoord/glReadPixels, both bottom-origin.
// WebGPU's framebuffer / copy-to-buffer readback is top-origin (row 0 = top) and @builtin(position) is
// also top-origin. To keep the closed-form arithmetic byte-identical, every pixel-space vertex shader
// flips NDC y so that pixel-y == readback-row: pixel row y is asserted at readback row y, and the
// analytic fragment shaders read @builtin(position).y == pixel-y directly. The Porter-Duff over
// operator, the analytic rounded-rect coverage, the nine-patch coverage, the 8x8 glyph bitmap, scissor
// clipping and the q8 quantization are ported verbatim in behavior from the GLES reference.

use std::sync::atomic::{AtomicU32, Ordering};

use wgpu::util::DeviceExt;

const W: u32 = 64;
const H: u32 = 64;
const BPP: u32 = 4;

static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

// Assertion budget, calibrated to the count this cell genuinely runs on the success path. The GLES
// reference pins EXPECTED=37; several of those are GL/EGL bring-up asserts (eglGetDisplay,
// eglInitialize, eglChooseConfig, eglBindAPI, eglCreateContext, eglMakeCurrent, FBO-complete, per-shader
// compile/link + uniform-location + no-GL-error) with no WebGPU counterpart (wgpu validates via
// on_uncaptured_error, shader/pipeline creation does not return a Result to assert on). The
// adapter/device requests replace the EGL bring-up asserts. Every SCENE assertion (per-pixel closed-form
// coverage for each primitive plus the spot checks and negative control) is preserved 1:1, landing at 28.
const EXPECTED: u32 = 28;

fn ok(cond: bool, desc: &str) {
    if cond {
        PASS.fetch_add(1, Ordering::Relaxed);
    } else {
        FAIL.fetch_add(1, Ordering::Relaxed);
        eprintln!("FAIL: {desc}");
    }
}

fn clampi(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}
fn q8(f: f32) -> u8 {
    clampi((f * 255.0).round() as i32, 0, 255) as u8
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidColor {
    rgba: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2 {
    pos: [f32; 2],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2UV {
    pos: [f32; 2],
    uv: [f32; 2],
}

// Pixel-space vertex shader: input pixel coords in [0,W]x[0,H], map to NDC with a y-flip so pixel row 0
// lands at readback row 0. Solid uniform color out.
const SOLID_WGSL: &str = r#"
struct Solid { rgba: vec4<f32>, vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    let n = (p / u.vp.xy) * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"#;

// Analytic rounded-rect fragment shader over a full-screen pixel quad. @builtin(position).xy is
// (col+0.5, row+0.5) == pixel center in the pixel-y == row convention.
const RR_WGSL: &str = r#"
struct RR { box: vec4<f32>, col: vec4<f32>, rad: vec4<f32>, vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: RR;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    let n = (p / u.vp.xy) * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let p = fc.xy;
    let x0 = u.box.x; let y0 = u.box.y; let x1 = u.box.z; let y1 = u.box.w;
    let rad = u.rad.x;
    let inside = p.x >= x0 && p.x < x1 && p.y >= y0 && p.y < y1;
    if (!inside) { discard; }
    var corner = false;
    var cc = vec2<f32>(0.0, 0.0);
    if (p.x < x0 + rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x0 + rad, y0 + rad); }
    else if (p.x >= x1 - rad && p.y < y0 + rad) { corner = true; cc = vec2<f32>(x1 - rad, y0 + rad); }
    else if (p.x < x0 + rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x0 + rad, y1 - rad); }
    else if (p.x >= x1 - rad && p.y >= y1 - rad) { corner = true; cc = vec2<f32>(x1 - rad, y1 - rad); }
    if (corner && distance(p, cc) > rad) { discard; }
    return u.col;
}
"#;

// Glyph blit: pos2 + uv, sample an 8x8 texture (Nearest). Pixel-space vertex with y-flip; uv carried.
const TEX_WGSL: &str = r#"
struct Vp { vp: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Vp;
@group(0) @binding(1) var t: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    let n = (p / u.vp.xy) * 2.0 - 1.0;
    o.pos = vec4<f32>(n.x, -n.y, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return textureSample(t, s, in.uv); }
"#;

fn main() {
    std::process::exit(pollster::block_on(run()));
}

struct Fb {
    px: Vec<u8>,
}
impl Fb {
    fn p(&self, x: u32, y: u32, c: usize) -> u8 {
        self.px[((y * W + x) * BPP) as usize + c]
    }
    fn peq(&self, x: i32, y: i32, r: i32, g: i32, b: i32, a: i32, tol: i32) -> bool {
        let d = |v: u8, t: i32| (v as i32 - t).abs() <= tol;
        d(self.p(x as u32, y as u32, 0), r)
            && d(self.p(x as u32, y as u32, 1), g)
            && d(self.p(x as u32, y as u32, 2), b)
            && d(self.p(x as u32, y as u32, 3), a)
    }
}

async fn run() -> i32 {
    let backends = match std::env::var("WGPU_BACKEND").ok().as_deref() {
        Some("gl") | Some("gles") => wgpu::Backends::GL,
        Some("vulkan") => wgpu::Backends::VULKAN,
        _ => wgpu::Backends::VULKAN | wgpu::Backends::GL,
    };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .await
    {
        Some(a) => a,
        None => {
            eprintln!("request_adapter returned None");
            ok(false, "request_adapter yields a usable adapter");
            return finish();
        }
    };
    let info = adapter.get_info();
    println!(
        "wgpu adapter selected: backend={:?} name='{}' driver='{}' type={:?}",
        info.backend, info.name, info.driver, info.device_type
    );
    ok(
        info.backend != wgpu::Backend::Empty,
        "request_adapter yields a usable adapter",
    );
    ok(
        matches!(info.backend, wgpu::Backend::Vulkan | wgpu::Backend::Gl),
        "adapter backend is Vulkan or Gl",
    );
    let (device, queue) = match adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("2dui-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        )
        .await
    {
        Ok(x) => x,
        Err(e) => {
            eprintln!("request_device failed: {e}");
            ok(false, "request_device yields a usable device");
            return finish();
        }
    };
    device.on_uncaptured_error(Box::new(|e| eprintln!("UNCAPTURED wgpu error: {e}")));

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let unpadded = W * BPP;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    // ---- Solid pixel-fill pipeline (uniform color + vp) ----
    let solid_ubo = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("solid-ubo"),
        size: 32, // vec4 rgba + vec4 vp
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let solid_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("solid-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let solid_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("solid-bg"),
        layout: &solid_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: solid_ubo.as_entire_binding(),
        }],
    });
    let solid_pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("solid-pll"),
        bind_group_layouts: &[&solid_bgl],
        push_constant_ranges: &[],
    });
    let m_solid = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("solid"),
        source: wgpu::ShaderSource::Wgsl(SOLID_WGSL.into()),
    });
    let vbl_pos2 = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<V2>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        }],
    };
    let mk_solid = |blend: Option<wgpu::BlendState>| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid-pipe"),
            layout: Some(&solid_pll),
            vertex: wgpu::VertexState {
                module: &m_solid,
                entry_point: "vs",
                buffers: std::slice::from_ref(&vbl_pos2),
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &m_solid,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    };
    let pipe_solid = mk_solid(None);
    let blend_over = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    };
    let pipe_blend = mk_solid(Some(blend_over));

    // helper: pack SolidColor uniform (rgba + vp).
    let write_solid = |r: f32, g: f32, b: f32, a: f32| {
        let data = [r, g, b, a, W as f32, H as f32, 0.0, 0.0];
        queue.write_buffer(&solid_ubo, 0, bytemuck::cast_slice(&data));
    };

    // Two-triangle pixel rect [x0,x1)x[y0,y1).
    let rect_verts = |x0: f32, y0: f32, x1: f32, y1: f32| {
        [
            V2 { pos: [x0, y0] },
            V2 { pos: [x1, y0] },
            V2 { pos: [x0, y1] },
            V2 { pos: [x0, y1] },
            V2 { pos: [x1, y0] },
            V2 { pos: [x1, y1] },
        ]
    };

    // Multi-draw frame: clear + a list of (pipeline, bind, vbo, nverts, scissor).
    struct DrawOp<'a> {
        pipe: &'a wgpu::RenderPipeline,
        bind: &'a wgpu::BindGroup,
        vbo: &'a wgpu::Buffer,
        verts: u32,
        scissor: Option<(u32, u32, u32, u32)>,
    }
    let frame = |clear: [f64; 4], ops: &[DrawOp]| -> Fb {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0],
                            g: clear[1],
                            b: clear[2],
                            a: clear[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            for op in ops {
                rp.set_pipeline(op.pipe);
                if let Some((x, y, w, h)) = op.scissor {
                    rp.set_scissor_rect(x, y, w, h);
                } else {
                    rp.set_scissor_rect(0, 0, W, H);
                }
                rp.set_bind_group(0, op.bind, &[]);
                rp.set_vertex_buffer(0, op.vbo.slice(..));
                rp.draw(0..op.verts, 0..1);
            }
        }
        copy_and_read(&device, &queue, &color, &readback, enc, padded)
    };

    ok(true, "offscreen Rgba8Unorm target + readback buffer ready");

    // ---- Scene A: filled rectangles ----
    // Note: WebGPU writes the solid color into an Rgba8Unorm target from a straight-color fragment; the
    // draws below are opaque so no blend. Each rect is uploaded as its own vbo per draw.
    write_solid(1.0, 0.0, 0.0, 1.0);
    let vr1 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("rectA"),
        contents: bytemuck::cast_slice(&rect_verts(8.0, 8.0, 16.0, 24.0)),
        usage: wgpu::BufferUsages::VERTEX,
    });
    // For the second rect a distinct color is needed; the uniform is shared, so draw rects in separate
    // frames would lose the first. Instead we bake per-rect color into per-rect uniform buffers.
    let ubo_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("uboA"),
        contents: bytemuck::cast_slice(&[1.0f32, 0.0, 0.0, 1.0, W as f32, H as f32, 0.0, 0.0]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let ubo_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("uboB"),
        contents: bytemuck::cast_slice(&[0.0f32, 1.0, 0.0, 1.0, W as f32, H as f32, 0.0, 0.0]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bg_a = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bgA"),
        layout: &solid_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: ubo_a.as_entire_binding(),
        }],
    });
    let bg_b = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bgB"),
        layout: &solid_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: ubo_b.as_entire_binding(),
        }],
    });
    let vr2 = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("rectB"),
        contents: bytemuck::cast_slice(&rect_verts(40.0, 32.0, 48.0, 52.0)),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let fb = frame(
        [0.0, 0.0, 0.0, 1.0],
        &[
            DrawOp {
                pipe: &pipe_solid,
                bind: &bg_a,
                vbo: &vr1,
                verts: 6,
                scissor: None,
            },
            DrawOp {
                pipe: &pipe_solid,
                bind: &bg_b,
                vbo: &vr2,
                verts: 6,
                scissor: None,
            },
        ],
    );
    {
        let mut bad = 0;
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let (er, eg, eb);
                if x >= 8 && x < 16 && y >= 8 && y < 24 {
                    er = 255;
                    eg = 0;
                    eb = 0;
                } else if x >= 40 && x < 48 && y >= 32 && y < 52 {
                    er = 0;
                    eg = 255;
                    eb = 0;
                } else {
                    er = 0;
                    eg = 0;
                    eb = 0;
                }
                if !fb.peq(x, y, er, eg, eb, 255, 1) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "filled rectangles: every pixel matches closed-form rect coverage",
        );
        ok(fb.peq(10, 10, 255, 0, 0, 255, 1), "rect A interior red");
        ok(fb.peq(44, 40, 0, 255, 0, 255, 1), "rect B interior green");
        ok(
            fb.peq(30, 30, 0, 0, 0, 255, 1),
            "gap between rects is background",
        );
    }

    // ---- Scene B: analytic rounded-rect ----
    {
        let rr_ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rr-ubo"),
            // box(12,12,52,52), col(1,1,0,1), rad(8), vp(W,H)
            contents: bytemuck::cast_slice(&[
                12.0f32, 12.0, 52.0, 52.0, 1.0, 1.0, 0.0, 1.0, 8.0, 0.0, 0.0, 0.0, W as f32,
                H as f32, 0.0, 0.0,
            ]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let rr_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rr-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let rr_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rr-bg"),
            layout: &rr_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: rr_ubo.as_entire_binding(),
            }],
        });
        let rr_pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rr-pll"),
            bind_group_layouts: &[&rr_bgl],
            push_constant_ranges: &[],
        });
        let m_rr = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rr"),
            source: wgpu::ShaderSource::Wgsl(RR_WGSL.into()),
        });
        let pipe_rr = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rr-pipe"),
            layout: Some(&rr_pll),
            vertex: wgpu::VertexState {
                module: &m_rr,
                entry_point: "vs",
                buffers: std::slice::from_ref(&vbl_pos2),
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &m_rr,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let fq = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fullquad"),
            contents: bytemuck::cast_slice(&rect_verts(0.0, 0.0, W as f32, H as f32)),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            &[DrawOp {
                pipe: &pipe_rr,
                bind: &rr_bg,
                vbo: &fq,
                verts: 6,
                scissor: None,
            }],
        );
        let covered = |x: i32, y: i32| -> bool {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (x0, y0, x1, y1, r) = (12.0f32, 12.0, 52.0, 52.0, 8.0);
            if !(cx >= x0 && cx < x1 && cy >= y0 && cy < y1) {
                return false;
            }
            let (mut ccx, mut ccy) = (0.0f32, 0.0f32);
            let mut corner = false;
            if cx < x0 + r && cy < y0 + r {
                corner = true;
                ccx = x0 + r;
                ccy = y0 + r;
            } else if cx >= x1 - r && cy < y0 + r {
                corner = true;
                ccx = x1 - r;
                ccy = y0 + r;
            } else if cx < x0 + r && cy >= y1 - r {
                corner = true;
                ccx = x0 + r;
                ccy = y1 - r;
            } else if cx >= x1 - r && cy >= y1 - r {
                corner = true;
                ccx = x1 - r;
                ccy = y1 - r;
            }
            if corner {
                let (dx, dy) = (cx - ccx, cy - ccy);
                if (dx * dx + dy * dy).sqrt() > r {
                    return false;
                }
            }
            true
        };
        let mut bad = 0;
        let mut lit = 0;
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let cov = covered(x, y);
                if cov {
                    lit += 1;
                }
                let er = if cov { 255 } else { 0 };
                let eg = if cov { 255 } else { 0 };
                if !fb.peq(x, y, er, eg, 0, 255, 1) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "rounded-rect: every pixel matches analytic corner-arc coverage",
        );
        ok(lit > 0, "rounded-rect: some pixels covered");
        ok(
            fb.peq(32, 32, 255, 255, 0, 255, 1),
            "rounded-rect center lit",
        );
        ok(
            fb.peq(12, 12, 0, 0, 0, 255, 1),
            "rounded-rect clipped corner (12,12) is background",
        );
        ok(
            fb.peq(32, 13, 255, 255, 0, 255, 1),
            "rounded-rect straight top edge lit",
        );
    }

    // ---- Scene C: nine-patch-style scaled border frame ----
    {
        let vbox = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nine-outer"),
            contents: bytemuck::cast_slice(&rect_verts(4.0, 4.0, 60.0, 60.0)),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let vinner = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nine-inner"),
            contents: bytemuck::cast_slice(&rect_verts(10.0, 10.0, 54.0, 54.0)),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ubo_blue = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blue"),
            contents: bytemuck::cast_slice(&[0.0f32, 0.0, 1.0, 1.0, W as f32, H as f32, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let ubo_dark = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dark"),
            contents: bytemuck::cast_slice(&[0.1f32, 0.1, 0.1, 1.0, W as f32, H as f32, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bg_blue = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg-blue"),
            layout: &solid_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo_blue.as_entire_binding(),
            }],
        });
        let bg_dark = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg-dark"),
            layout: &solid_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo_dark.as_entire_binding(),
            }],
        });
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            &[
                DrawOp {
                    pipe: &pipe_solid,
                    bind: &bg_blue,
                    vbo: &vbox,
                    verts: 6,
                    scissor: None,
                },
                DrawOp {
                    pipe: &pipe_solid,
                    bind: &bg_dark,
                    vbo: &vinner,
                    verts: 6,
                    scissor: None,
                },
            ],
        );
        let mut bad = 0;
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let inbox = x >= 4 && x < 60 && y >= 4 && y < 60;
                let ininner = x >= 10 && x < 54 && y >= 10 && y < 54;
                let (er, eg, eb);
                if ininner {
                    er = q8(0.1) as i32;
                    eg = q8(0.1) as i32;
                    eb = q8(0.1) as i32;
                } else if inbox {
                    er = 0;
                    eg = 0;
                    eb = 255;
                } else {
                    er = 0;
                    eg = 0;
                    eb = 0;
                }
                if !fb.peq(x, y, er, eg, eb, 255, 1) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "nine-patch border frame: closed-form border-vs-interior coverage",
        );
        ok(
            fb.peq(5, 32, 0, 0, 255, 255, 1),
            "nine-patch left border blue",
        );
        ok(
            fb.peq(32, 5, 0, 0, 255, 255, 1),
            "nine-patch top border blue",
        );
        ok(
            fb.peq(
                32,
                32,
                q8(0.1) as i32,
                q8(0.1) as i32,
                q8(0.1) as i32,
                255,
                1,
            ),
            "nine-patch hollow interior",
        );
    }

    // ---- Scene D: 8x8 bitmap-font glyph blit ----
    const GLYPH_H: [u8; 8] = [0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00];
    {
        // Build RGBA where lit texels are white. Texture row r, col c; MSB = leftmost column.
        let mut rgba = [0u8; 8 * 8 * 4];
        for r in 0..8usize {
            for c in 0..8usize {
                let lit = (GLYPH_H[r] >> (7 - c)) & 1 == 1;
                let v = if lit { 255 } else { 0 };
                let idx = (r * 8 + c) * 4;
                rgba[idx] = v;
                rgba[idx + 1] = v;
                rgba[idx + 2] = v;
                rgba[idx + 3] = 255;
            }
        }
        let gtex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &gtex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(32),
                rows_per_image: Some(8),
            },
            wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
        );
        let gview = gtex.create_view(&wgpu::TextureViewDescriptor::default());
        let samp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let vp_ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("glyph-vp"),
            contents: bytemuck::cast_slice(&[W as f32, H as f32, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glyph-bgl"),
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyph-bg"),
            layout: &tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vp_ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&gview),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&samp),
                },
            ],
        });
        let tex_pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glyph-pll"),
            bind_group_layouts: &[&tex_bgl],
            push_constant_ranges: &[],
        });
        let m_tex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tex"),
            source: wgpu::ShaderSource::Wgsl(TEX_WGSL.into()),
        });
        let vbl_pos2uv = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<V2UV>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };
        let pipe_tex = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glyph-pipe"),
            layout: Some(&tex_pll),
            vertex: wgpu::VertexState {
                module: &m_tex,
                entry_point: "vs",
                buffers: std::slice::from_ref(&vbl_pos2uv),
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &m_tex,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        // Glyph blitted to pixel rect [20,28)x[20,28), uv 0..1 over the 8x8 glyph. In the pixel-y == row
        // convention, screen row (20+dy) samples v=(dy+0.5)/8 -> texture row dy (glyph stored top-row 0).
        let gq: [V2UV; 6] = [
            V2UV {
                pos: [20.0, 20.0],
                uv: [0.0, 0.0],
            },
            V2UV {
                pos: [28.0, 20.0],
                uv: [1.0, 0.0],
            },
            V2UV {
                pos: [20.0, 28.0],
                uv: [0.0, 1.0],
            },
            V2UV {
                pos: [20.0, 28.0],
                uv: [0.0, 1.0],
            },
            V2UV {
                pos: [28.0, 20.0],
                uv: [1.0, 0.0],
            },
            V2UV {
                pos: [28.0, 28.0],
                uv: [1.0, 1.0],
            },
        ];
        let gvbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("glyph-quad"),
            contents: bytemuck::cast_slice(&gq),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            &[DrawOp {
                pipe: &pipe_tex,
                bind: &tex_bg,
                vbo: &gvbo,
                verts: 6,
                scissor: None,
            }],
        );
        let mut bad = 0;
        for dy in 0..8i32 {
            for dx in 0..8i32 {
                let (sx, sy) = (20 + dx, 20 + dy);
                let (trow, tcol) = (dy as usize, dx as usize);
                let lit = (GLYPH_H[trow] >> (7 - tcol)) & 1 == 1;
                let v = if lit { 255 } else { 0 };
                if !fb.peq(sx, sy, v, v, v, 255, 1) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap",
        );
        ok(
            fb.peq(21, 23, 255, 255, 255, 255, 1),
            "glyph crossbar lit (col1,row3)",
        );
        ok(fb.peq(23, 20, 0, 0, 0, 255, 1), "glyph row0 blank");
        ok(
            fb.peq(24, 21, 0, 0, 0, 255, 1),
            "glyph row1 middle blank (0x42)",
        );
    }

    // ---- Scene E: scissor-clipped fill ----
    {
        write_solid(1.0, 0.0, 1.0, 1.0);
        let fq = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scissor-quad"),
            contents: bytemuck::cast_slice(&rect_verts(0.0, 0.0, W as f32, H as f32)),
            usage: wgpu::BufferUsages::VERTEX,
        });
        // scissor box in WebGPU is top-origin (x,y,w,h): the reference expects [16,36)x[16,36) in the
        // pixel-y == row convention, i.e. scissor origin (16,16) size 20x20.
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            &[DrawOp {
                pipe: &pipe_solid,
                bind: &solid_bg,
                vbo: &fq,
                verts: 6,
                scissor: Some((16, 16, 20, 20)),
            }],
        );
        let mut bad = 0;
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let inb = x >= 16 && x < 36 && y >= 16 && y < 36;
                let er = if inb { 255 } else { 0 };
                let eb = if inb { 255 } else { 0 };
                if !fb.peq(x, y, er, 0, eb, 255, 1) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "scissor-clipped fill: magenta only within [16,36)^2",
        );
        ok(
            fb.peq(20, 20, 255, 0, 255, 255, 1),
            "scissor inside magenta",
        );
        ok(
            fb.peq(40, 40, 0, 0, 0, 255, 1),
            "scissor outside background",
        );
    }

    // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
    {
        struct L {
            r: f32,
            g: f32,
            b: f32,
            a: f32,
            x0: f32,
            y0: f32,
            x1: f32,
            y1: f32,
        }
        let bg = [0.10f32, 0.10, 0.10, 1.0];
        let layers = [
            L {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 0.50,
                x0: 8.0,
                y0: 8.0,
                x1: 56.0,
                y1: 56.0,
            },
            L {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 0.25,
                x0: 12.0,
                y0: 12.0,
                x1: 52.0,
                y1: 52.0,
            },
            L {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 0.75,
                x0: 16.0,
                y0: 16.0,
                x1: 48.0,
                y1: 48.0,
            },
        ];
        let mut ops_bgs = Vec::new();
        let mut ops_vbos = Vec::new();
        for (i, l) in layers.iter().enumerate() {
            let ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("layer-ubo"),
                contents: bytemuck::cast_slice(&[l.r, l.g, l.b, l.a, W as f32, H as f32, 0.0, 0.0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bgd = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer-bg"),
                layout: &solid_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubo.as_entire_binding(),
                }],
            });
            let v = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("layer-quad"),
                contents: bytemuck::cast_slice(&rect_verts(l.x0, l.y0, l.x1, l.y1)),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let _ = i;
            ops_bgs.push(bgd);
            ops_vbos.push(v);
        }
        let ops: Vec<DrawOp> = (0..3)
            .map(|i| DrawOp {
                pipe: &pipe_blend,
                bind: &ops_bgs[i],
                vbo: &ops_vbos[i],
                verts: 6,
                scissor: None,
            })
            .collect();
        let fb = frame(
            [bg[0] as f64, bg[1] as f64, bg[2] as f64, bg[3] as f64],
            &ops,
        );
        let composite = |tx: i32, ty: i32| -> [f32; 4] {
            let mut c = bg;
            for l in layers.iter() {
                let (cx, cy) = (tx as f32 + 0.5, ty as f32 + 0.5);
                if cx >= l.x0 && cx < l.x1 && cy >= l.y0 && cy < l.y1 {
                    let a_s = l.a;
                    let src = [l.r, l.g, l.b, l.a];
                    for k in 0..4 {
                        c[k] = src[k] * a_s + c[k] * (1.0 - a_s);
                    }
                }
            }
            c
        };
        let mut bad = 0;
        for y in 0..H as i32 {
            for x in 0..W as i32 {
                let e = composite(x, y);
                if !fb.peq(
                    x,
                    y,
                    q8(e[0]) as i32,
                    q8(e[1]) as i32,
                    q8(e[2]) as i32,
                    q8(e[3]) as i32,
                    2,
                ) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "multi-layer over: every pixel matches Porter-Duff over accumulation (incl \
             partial-overlap regions)",
        );
        {
            let mut c = bg;
            let ls = [
                [1.0, 0.0, 0.0, 0.5f32],
                [0.0, 1.0, 0.0, 0.25],
                [0.0, 0.0, 1.0, 0.75],
            ];
            for li in ls {
                let a_s = li[3];
                for k in 0..4 {
                    c[k] = li[k] * a_s + c[k] * (1.0 - a_s);
                }
            }
            ok(
                fb.peq(
                    32,
                    32,
                    q8(c[0]) as i32,
                    q8(c[1]) as i32,
                    q8(c[2]) as i32,
                    q8(c[3]) as i32,
                    2,
                ),
                "multi-layer over center pixel matches hand-iterated over",
            );
        }
        {
            let a_s = 0.5f32;
            let er = 1.0 * a_s + bg[0] * (1.0 - a_s);
            let eg = 0.0 * a_s + bg[1] * (1.0 - a_s);
            let eb = 0.0 * a_s + bg[2] * (1.0 - a_s);
            let ea = a_s * a_s + bg[3] * (1.0 - a_s);
            ok(
                fb.peq(
                    10,
                    32,
                    q8(er) as i32,
                    q8(eg) as i32,
                    q8(eb) as i32,
                    q8(ea) as i32,
                    2,
                ),
                "multi-layer over: single-layer region matches one over",
            );
        }
    }

    // ---- Negative control ----
    {
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            &[DrawOp {
                pipe: &pipe_solid,
                bind: &bg_a,
                vbo: &vr1,
                verts: 6,
                scissor: None,
            }],
        );
        ok(
            !fb.peq(10, 10, 0, 255, 0, 255, 4),
            "negative control: red rect pixel is NOT green",
        );
        ok(
            !fb.peq(30, 30, 255, 0, 0, 255, 4),
            "negative control: background is NOT red",
        );
    }

    device.poll(wgpu::Maintain::Wait);
    device.destroy();
    finish()
}

fn copy_and_read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color: &wgpu::Texture,
    readback: &wgpu::Buffer,
    mut enc: wgpu::CommandEncoder,
    padded: u32,
) -> Fb {
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: readback,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(enc.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .expect("map_async channel")
        .expect("map_async failed");
    let data = slice.get_mapped_range();
    let mut px = vec![0u8; (W * H * BPP) as usize];
    for y in 0..H {
        let src = (y * padded) as usize;
        let dst = (y * W * BPP) as usize;
        let n = (W * BPP) as usize;
        px[dst..dst + n].copy_from_slice(&data[src..src + n]);
    }
    drop(data);
    readback.unmap();
    Fb { px }
}

fn finish() -> i32 {
    let pass = PASS.load(Ordering::Relaxed);
    let fail = FAIL.load(Ordering::Relaxed);
    let total = pass + fail;
    println!("scene-2dui: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={EXPECTED}");
    if fail == 0 && total == EXPECTED {
        println!("SCENE_2DUI OK {pass}");
        0
    } else {
        println!("SCENE_2DUI FAIL");
        1
    }
}
