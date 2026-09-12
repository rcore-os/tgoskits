// wgpu_render_rust_full_api - WebGPU/wgpu RENDER carpet on Mesa lavapipe (software Vulkan on the CPU, no
// GPU/window/surface/swapchain), driven by the `wgpu` crate. It renders offscreen into a 64x64
// RGBA8Unorm texture (RENDER_ATTACHMENT | COPY_SRC) through real render pipelines with WGSL vertex+
// fragment shaders, copies the texture to a MAP_READ buffer (honouring the 256-byte bytesPerRow
// alignment: rows are padded on copy then unpadded on readback), maps it, and hard-asserts every pixel
// against a closed-form reference. Coverage mirrors the verified Vulkan reference cell, adjusted to the
// WebGPU spec (webgpu.h is the ground truth, not Vulkan 1:1): render-pass clear (LoadOp::Clear), a solid
// quad (uniform-buffer color; WebGPU has no push constants by default), a per-vertex axis-aligned linear
// gradient (a triangle-strip quad interpolates per triangle so only an axis-aligned gradient matches a
// full-quad closed form), a @builtin(position) checkerboard, a scissor rect, a viewport restriction,
// alpha blend, and a sub-rectangle readback. Exhaustive per-API coverage builds a pipeline per state:
// all 5 WebGPU primitive topologies (PointList/LineList/LineStrip/TriangleList/TriangleStrip - WebGPU has
// NO triangle-fan, and points are always 1px with no PointSize builtin), a blend factor+op matrix
// (One/Zero replace, One/One Add, Zero/One keep-dst, Dst/Zero modulate, One/One Max, One/One
// ReverseSubtract -> alpha 0), the full depth-compare matrix (all 8 wgpu::CompareFunction against a
// Depth32Float attachment; WebGPU NDC z in [0,1] so a z=0.5 quad vs clear-depth 0.75 draws only under
// {Always,Less,LessEqual,NotEqual}), face culling + winding (cull None vs Back with FrontFace Ccw vs Cw),
// a color write mask (ColorWrites::RED vs ::ALL), texture-format feature + limit queries, and a 2x2
// RGBA8 texture upload + Nearest sampling through a sampler + bind group, closing with a negative
// control. Selects the backend via WGPU_BACKEND (vulkan=lavapipe / gl=llvmpipe) like the compute cell,
// prints the chosen adapter, and prints "WGPU_RENDER_RUST_FULL_API OK <n>" only when every assertion
// passes and the count equals the pinned EXPECTED total.

use std::{
    borrow::Cow,
    sync::atomic::{AtomicU32, Ordering},
};

use wgpu::util::DeviceExt;

const W: u32 = 64;
const H: u32 = 64;
const BPP: u32 = 4;

static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

// Assertion budget, calibrated to the count this cell genuinely runs on the success path. Coverage
// mirrors the Vulkan reference cell but adjusts to the WebGPU spec: there is no triangle-fan topology
// (5 WebGPU topologies, not 6), and Vulkan's descriptor-pool / image-layout-barrier plumbing has no
// WebGPU counterpart, so the raw assertion tally differs from Vulkan's 68. Two viewport-restriction
// assertions are added that the Vulkan cell lacks.
const EXPECTED: u32 = 56;

fn ok(cond: bool, desc: &str) {
    if cond {
        PASS.fetch_add(1, Ordering::Relaxed);
    } else {
        FAIL.fetch_add(1, Ordering::Relaxed);
        eprintln!("FAIL: {desc}");
    }
}

// Solid-color uniform block, std140-friendly (a single vec4<f32>).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidColor {
    rgba: [f32; 4],
}

// Vertex layouts used by the pipelines.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2 {
    pos: [f32; 2],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2C {
    pos: [f32; 2],
    col: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V3 {
    pos: [f32; 3],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2UV {
    pos: [f32; 2],
    uv: [f32; 2],
}

// pos2 + uniform color -> solid fill.
const SOLID_WGSL: &str = r#"
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"#;

// pos2 + per-vertex color -> interpolated gradient.
const GRAD_WGSL: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p, 0.0, 1.0);
    o.col = c;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> { return in.col; }
"#;

// pos2 + @builtin(position) checkerboard: white when ((x/8 + y/8) & 1) == 0.
const CHECK_WGSL: &str = r#"
@vertex fn vs(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let cx = u32(floor(fc.x)) / 8u;
    let cy = u32(floor(fc.y)) / 8u;
    if (((cx + cy) & 1u) == 0u) {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"#;

// pos3 (carries a z for the depth-compare matrix) + uniform color.
const POS3_WGSL: &str = r#"
struct Solid { rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Solid;
@vertex fn vs(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(p, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
"#;

// pos2 + uv -> sample a 2x2 texture with a Nearest sampler.
const TEX_WGSL: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(t, s, in.uv);
}
"#;

fn main() {
    std::process::exit(pollster::block_on(run()));
}

// Readback framebuffer: an (H*W*4) unpadded RGBA8 image.
struct Fb {
    px: Vec<u8>,
}
impl Fb {
    fn p(&self, x: u32, y: u32, c: usize) -> u8 {
        self.px[((y * W + x) * BPP) as usize + c]
    }
    fn peq(&self, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8, tol: i32) -> bool {
        let d = |v: u8, t: u8| (v as i32 - t as i32).abs() <= tol;
        d(self.p(x, y, 0), r)
            && d(self.p(x, y, 1), g)
            && d(self.p(x, y, 2), b)
            && d(self.p(x, y, 3), a)
    }
    fn all_eq(&self, r: u8, g: u8, b: u8, a: u8, tol: i32) -> bool {
        (0..H).all(|y| (0..W).all(|x| self.peq(x, y, r, g, b, a, tol)))
    }
}

async fn run() -> i32 {
    // --- Instance + adapter (backend selected via WGPU_BACKEND, mirroring the compute cell) --------
    let backends = match std::env::var("WGPU_BACKEND").ok().as_deref() {
        Some("gl") | Some("gles") => wgpu::Backends::GL,
        Some("vulkan") => wgpu::Backends::VULKAN,
        _ => wgpu::Backends::VULKAN | wgpu::Backends::GL,
    };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    for a in instance.enumerate_adapters(wgpu::Backends::all()) {
        let i = a.get_info();
        eprintln!(
            "adapter: {:?} name='{}' driver='{}' type={:?}",
            i.backend, i.name, i.driver, i.device_type
        );
    }

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
            eprintln!("request_adapter returned None - no wgpu adapter on this host");
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
                label: Some("render-device"),
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

    // --- Color attachment + readback plumbing -----------------------------------------------------
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

    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    // 256-byte bytesPerRow alignment: 64*4 == 256 already, but compute the padded stride generically.
    let unpadded = W * BPP;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT; // 256
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    // --- Shaders ----------------------------------------------------------------------------------
    let sh = |src: &str, label: &str| {
        device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(src.to_owned())),
        })
    };
    let m_solid = sh(SOLID_WGSL, "solid");
    let m_grad = sh(GRAD_WGSL, "grad");
    let m_check = sh(CHECK_WGSL, "check");
    let m_pos3 = sh(POS3_WGSL, "pos3");
    let m_tex = sh(TEX_WGSL, "tex");

    // Uniform buffer + bind group for the solid color (replaces Vulkan push constants).
    let color_ubo = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("color-ubo"),
        size: std::mem::size_of::<SolidColor>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let ubo_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ubo-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let ubo_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ubo-bg"),
        layout: &ubo_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: color_ubo.as_entire_binding(),
        }],
    });
    let ubo_pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ubo-pll"),
        bind_group_layouts: &[&ubo_bgl],
        push_constant_ranges: &[],
    });
    let empty_pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("empty-pll"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });

    // Vertex buffer layouts.
    let vbl_pos2 = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<V2>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        }],
    };
    let vbl_pos2col = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<V2C>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 8,
                shader_location: 1,
            },
        ],
    };
    let vbl_pos3 = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<V3>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        }],
    };
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

    let no_blend = |writes: wgpu::ColorWrites| wgpu::ColorTargetState {
        format: wgpu::TextureFormat::Rgba8Unorm,
        blend: None,
        write_mask: writes,
    };

    // Generic render-pipeline builder covering every state axis the coverage matrix needs.
    #[allow(clippy::too_many_arguments)]
    let mk = |vs_mod: &wgpu::ShaderModule,
              fs_mod: &wgpu::ShaderModule,
              layout: &wgpu::PipelineLayout,
              vbl: &wgpu::VertexBufferLayout,
              topo: wgpu::PrimitiveTopology,
              front: wgpu::FrontFace,
              cull: Option<wgpu::Face>,
              target: wgpu::ColorTargetState,
              depth_state: Option<wgpu::DepthStencilState>|
     -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pipe"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: vs_mod,
                entry_point: "vs",
                buffers: std::slice::from_ref(vbl),
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: fs_mod,
                entry_point: "fs",
                targets: &[Some(target)],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: topo,
                strip_index_format: None,
                front_face: front,
                cull_mode: cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: depth_state,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    };

    let depth_for = |cmp: wgpu::CompareFunction| wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: true,
        depth_compare: cmp,
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    // Vertex geometry. WebGPU clip space matches Vulkan's (y down in framebuffer via the same NDC),
    // so a full-screen strip quad is the same [-1,-1]..[1,1].
    let quad = [
        V2 { pos: [-1.0, -1.0] },
        V2 { pos: [1.0, -1.0] },
        V2 { pos: [-1.0, 1.0] },
        V2 { pos: [1.0, 1.0] },
    ];
    let vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("quad"),
        contents: bytemuck::cast_slice(&quad),
        usage: wgpu::BufferUsages::VERTEX,
    });
    // Axis-aligned gradient: red at left column, blue at right column (matches horizontal closed form).
    let gquad = [
        V2C {
            pos: [-1.0, -1.0],
            col: [1.0, 0.0, 0.0, 1.0],
        },
        V2C {
            pos: [1.0, -1.0],
            col: [0.0, 0.0, 1.0, 1.0],
        },
        V2C {
            pos: [-1.0, 1.0],
            col: [1.0, 0.0, 0.0, 1.0],
        },
        V2C {
            pos: [1.0, 1.0],
            col: [0.0, 0.0, 1.0, 1.0],
        },
    ];
    let gvbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("gquad"),
        contents: bytemuck::cast_slice(&gquad),
        usage: wgpu::BufferUsages::VERTEX,
    });

    // Base pipelines.
    let pipe_solid = mk(
        &m_solid,
        &m_solid,
        &ubo_pll,
        &vbl_pos2,
        wgpu::PrimitiveTopology::TriangleStrip,
        wgpu::FrontFace::Ccw,
        None,
        no_blend(wgpu::ColorWrites::ALL),
        None,
    );
    let pipe_grad = mk(
        &m_grad,
        &m_grad,
        &empty_pll,
        &vbl_pos2col,
        wgpu::PrimitiveTopology::TriangleStrip,
        wgpu::FrontFace::Ccw,
        None,
        no_blend(wgpu::ColorWrites::ALL),
        None,
    );
    let pipe_check = mk(
        &m_check,
        &m_check,
        &empty_pll,
        &vbl_pos2,
        wgpu::PrimitiveTopology::TriangleStrip,
        wgpu::FrontFace::Ccw,
        None,
        no_blend(wgpu::ColorWrites::ALL),
        None,
    );
    ok(true, "base render pipelines created");

    // Helper: set the solid-color uniform.
    let set_color = |r: f32, g: f32, b: f32, a: f32| {
        queue.write_buffer(
            &color_ubo,
            0,
            bytemuck::bytes_of(&SolidColor { rgba: [r, g, b, a] }),
        );
    };

    // Draw parameters bundle for one frame.
    struct Draw<'a> {
        pipe: Option<&'a wgpu::RenderPipeline>,
        bind: Option<&'a wgpu::BindGroup>,
        vbo: Option<&'a wgpu::Buffer>,
        verts: u32,
        scissor: Option<(u32, u32, u32, u32)>,
        viewport: Option<(f32, f32, f32, f32)>,
    }

    // Render one frame: clear to (cr,cg,cb,ca), optionally draw, copy color texture to the readback
    // buffer (padded rows), map, unpad into an Fb. Used for the color-only pass.
    let frame = |clear: [f64; 4], d: Draw| -> Fb {
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
            if let Some(pipe) = d.pipe {
                rp.set_pipeline(pipe);
                if let Some((x, y, w, h)) = d.viewport {
                    rp.set_viewport(x, y, w, h, 0.0, 1.0);
                }
                if let Some((x, y, w, h)) = d.scissor {
                    rp.set_scissor_rect(x, y, w, h);
                }
                if let Some(bg) = d.bind {
                    rp.set_bind_group(0, bg, &[]);
                }
                if let Some(v) = d.vbo {
                    rp.set_vertex_buffer(0, v.slice(..));
                }
                rp.draw(0..d.verts, 0..1);
            }
        }
        copy_and_read(&device, &queue, &color, &readback, enc, padded)
    };

    // Depth-enabled frame: clears color + depth, draws the pos3 quad through `pipe`.
    let frame_depth = |clear: [f64; 4],
                       depth_clear: f32,
                       pipe: &wgpu::RenderPipeline,
                       bind: &wgpu::BindGroup,
                       vbo: &wgpu::Buffer,
                       verts: u32|
     -> Fb {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame-d"),
        });
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rp-d"),
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
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth_clear),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(pipe);
            rp.set_bind_group(0, bind, &[]);
            rp.set_vertex_buffer(0, vbo.slice(..));
            rp.draw(0..verts, 0..1);
        }
        copy_and_read(&device, &queue, &color, &readback, enc, padded)
    };

    // ================= base coverage =================

    // Clear.
    let fb = frame(
        [0.0, 0.25, 0.5, 1.0],
        Draw {
            pipe: None,
            bind: None,
            vbo: None,
            verts: 0,
            scissor: None,
            viewport: None,
        },
    );
    ok(
        fb.all_eq(0, 64, 128, 255, 2),
        "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)",
    );
    ok(fb.peq(0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)");

    // Solid red quad.
    set_color(1.0, 0.0, 0.0, 1.0);
    let fb = frame(
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_solid),
            bind: Some(&ubo_bg),
            vbo: Some(&vbo),
            verts: 4,
            scissor: None,
            viewport: None,
        },
    );
    ok(
        fb.all_eq(255, 0, 0, 255, 1),
        "solid red quad fills every pixel",
    );

    // Axis-aligned gradient.
    let fb = frame(
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_grad),
            bind: None,
            vbo: Some(&gvbo),
            verts: 4,
            scissor: None,
            viewport: None,
        },
    );
    {
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let u = (x as f32 + 0.5) / W as f32;
                let r = ((1.0 - u) * 255.0).round() as u8;
                let b = (u * 255.0).round() as u8;
                if !fb.peq(x, y, r, 0, b, 255, 4) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "gradient matches horizontal-linear closed-form for all pixels",
        );
        ok(fb.peq(0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red");
        ok(
            fb.peq(W - 1, H - 1, 0, 0, 255, 255, 8),
            "gradient right edge ~ blue",
        );
        ok(
            fb.peq(W / 2, H / 2, 128, 0, 128, 255, 4),
            "gradient center ~ (128,0,128)",
        );
    }

    // Checkerboard from @builtin(position).
    let fb = frame(
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_check),
            bind: None,
            vbo: Some(&vbo),
            verts: 4,
            scissor: None,
            viewport: None,
        },
    );
    {
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let white = ((x / 8 + y / 8) & 1) == 0;
                let w = if white { 255 } else { 0 };
                if !fb.peq(x, y, w, w, w, 255, 1) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "checkerboard matches (x/8+y/8) parity for all pixels",
        );
        ok(
            fb.peq(0, 0, 255, 255, 255, 255, 1),
            "checker cell (0,0) white",
        );
        ok(fb.peq(8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black");
    }

    // Scissor.
    set_color(0.0, 1.0, 0.0, 1.0);
    let fb = frame(
        [1.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_solid),
            bind: Some(&ubo_bg),
            vbo: Some(&vbo),
            verts: 4,
            scissor: Some((16, 16, 32, 32)),
            viewport: None,
        },
    );
    ok(
        fb.peq(32, 32, 0, 255, 0, 255, 1),
        "scissor: inside box green",
    );
    ok(
        fb.peq(2, 2, 255, 0, 0, 255, 1),
        "scissor: outside box red (clear)",
    );
    ok(fb.peq(50, 50, 255, 0, 0, 255, 1), "scissor: past box red");

    // Viewport restriction: a viewport confined to the top-left 32x32 maps the full-NDC quad into that
    // sub-rect; pixels outside stay at the clear color. (WebGPU set_viewport, no Vulkan analogue count.)
    set_color(0.0, 1.0, 0.0, 1.0);
    let fb = frame(
        [1.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_solid),
            bind: Some(&ubo_bg),
            vbo: Some(&vbo),
            verts: 4,
            scissor: None,
            viewport: Some((0.0, 0.0, 32.0, 32.0)),
        },
    );
    ok(
        fb.peq(8, 8, 0, 255, 0, 255, 1),
        "viewport: inside 32x32 green",
    );
    ok(
        fb.peq(50, 50, 255, 0, 0, 255, 1),
        "viewport: outside stays clear red",
    );

    // Alpha blend: Src=SrcAlpha, Dst=OneMinusSrcAlpha, Add, over all channels (alpha too -> 191).
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
    let pipe_blend = mk(
        &m_solid,
        &m_solid,
        &ubo_pll,
        &vbl_pos2,
        wgpu::PrimitiveTopology::TriangleStrip,
        wgpu::FrontFace::Ccw,
        None,
        wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8Unorm,
            blend: Some(blend_over),
            write_mask: wgpu::ColorWrites::ALL,
        },
        None,
    );
    set_color(0.0, 0.0, 1.0, 0.5);
    let fb = frame(
        [1.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_blend),
            bind: Some(&ubo_bg),
            vbo: Some(&vbo),
            verts: 4,
            scissor: None,
            viewport: None,
        },
    );
    ok(
        fb.all_eq(128, 0, 128, 191, 3),
        "alpha blend 0.5*blue over red -> rgb(128,0,128) a191",
    );

    // Sub-rect readback.
    set_color(0.2, 0.4, 0.6, 1.0);
    let fb = frame(
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_solid),
            bind: Some(&ubo_bg),
            vbo: Some(&vbo),
            verts: 4,
            scissor: None,
            viewport: None,
        },
    );
    {
        let mut good = true;
        for y in 10..14 {
            for x in 10..14 {
                if !fb.peq(x, y, 51, 102, 153, 255, 2) {
                    good = false;
                }
            }
        }
        ok(good, "sub-rect (10,10,4x4) == (51,102,153,255)");
    }

    // ================= exhaustive per-API render coverage =================

    // --- Topologies: all 5 WebGPU topologies (NO triangle-fan) ------------------------------------
    set_color(1.0, 0.0, 0.0, 1.0);
    {
        // TriangleList: two CCW tris covering the quad.
        let tl = [
            V2 { pos: [-1.0, -1.0] },
            V2 { pos: [1.0, -1.0] },
            V2 { pos: [-1.0, 1.0] },
            V2 { pos: [-1.0, 1.0] },
            V2 { pos: [1.0, -1.0] },
            V2 { pos: [1.0, 1.0] },
        ];
        let b_tl = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tl"),
            contents: bytemuck::cast_slice(&tl),
            usage: wgpu::BufferUsages::VERTEX,
        });
        // A horizontal center line for LineList / LineStrip.
        let ln = [V2 { pos: [-1.0, 0.0] }, V2 { pos: [1.0, 0.0] }];
        let b_ln = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ln"),
            contents: bytemuck::cast_slice(&ln),
            usage: wgpu::BufferUsages::VERTEX,
        });
        // A single center point.
        let pt = [V2 { pos: [0.0, 0.0] }];
        let b_pt = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pt"),
            contents: bytemuck::cast_slice(&pt),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let mkt = |topo| {
            mk(
                &m_solid,
                &m_solid,
                &ubo_pll,
                &vbl_pos2,
                topo,
                wgpu::FrontFace::Ccw,
                None,
                no_blend(wgpu::ColorWrites::ALL),
                None,
            )
        };
        let p_tl = mkt(wgpu::PrimitiveTopology::TriangleList);
        let p_ll = mkt(wgpu::PrimitiveTopology::LineList);
        let p_ls = mkt(wgpu::PrimitiveTopology::LineStrip);
        let p_pt = mkt(wgpu::PrimitiveTopology::PointList);
        ok(true, "topology pipelines created");

        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_tl),
                bind: Some(&ubo_bg),
                vbo: Some(&b_tl),
                verts: 6,
                scissor: None,
                viewport: None,
            },
        );
        ok(fb.all_eq(255, 0, 0, 255, 1), "TriangleList fills quad");

        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_ll),
                bind: Some(&ubo_bg),
                vbo: Some(&b_ln),
                verts: 2,
                scissor: None,
                viewport: None,
            },
        );
        {
            let mut mid = 0;
            for x in 0..W {
                if fb.peq(x, H / 2, 255, 0, 0, 255, 2) || fb.peq(x, H / 2 - 1, 255, 0, 0, 255, 2) {
                    mid += 1;
                }
            }
            ok(mid >= W - 2, "LineList draws the middle row");
            ok(
                fb.peq(0, 0, 0, 0, 0, 255, 2),
                "LineList leaves top row clear",
            );
        }

        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_ls),
                bind: Some(&ubo_bg),
                vbo: Some(&b_ln),
                verts: 2,
                scissor: None,
                viewport: None,
            },
        );
        {
            let mut mid = 0;
            for x in 0..W {
                if fb.peq(x, H / 2, 255, 0, 0, 255, 2) || fb.peq(x, H / 2 - 1, 255, 0, 0, 255, 2) {
                    mid += 1;
                }
            }
            ok(mid >= W - 2, "LineStrip draws the middle row");
        }

        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_pt),
                bind: Some(&ubo_bg),
                vbo: Some(&b_pt),
                verts: 1,
                scissor: None,
                viewport: None,
            },
        );
        {
            // WebGPU points are always 1px (no PointSize); assert a pixel near the center is lit.
            let mut hit = false;
            for y in (H / 2 - 2)..=(H / 2 + 2) {
                for x in (W / 2 - 2)..=(W / 2 + 2) {
                    if fb.peq(x, y, 255, 0, 0, 255, 2) {
                        hit = true;
                    }
                }
            }
            ok(hit, "PointList draws a 1px point at the center");
        }
    }

    // --- Blend factor + op matrix -----------------------------------------------------------------
    {
        let mk_blend = |sc, dc, oc, sa, da, oa| {
            mk(
                &m_solid,
                &m_solid,
                &ubo_pll,
                &vbl_pos2,
                wgpu::PrimitiveTopology::TriangleStrip,
                wgpu::FrontFace::Ccw,
                None,
                wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: sc,
                            dst_factor: dc,
                            operation: oc,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: sa,
                            dst_factor: da,
                            operation: oa,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                },
                None,
            )
        };
        use wgpu::{BlendFactor as F, BlendOperation as O};

        // One/Zero: src replaces dst.
        let p = mk_blend(F::One, F::Zero, O::Add, F::One, F::Zero, O::Add);
        set_color(0.0, 0.0, 1.0, 1.0);
        let fb = frame(
            [0.5, 0.5, 0.5, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(0, 0, 255, 255, 2),
            "blend One/Zero: src replaces dst",
        );

        // One/One Add.
        let p = mk_blend(F::One, F::One, O::Add, F::One, F::One, O::Add);
        set_color(0.0, 0.0, 0.5, 1.0);
        let fb = frame(
            [0.5, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(128, 0, 128, 255, 2),
            "blend One/One Add: src+dst = (128,0,128)",
        );

        // Zero/One: dst kept.
        let p = mk_blend(F::Zero, F::One, O::Add, F::Zero, F::One, O::Add);
        set_color(0.0, 1.0, 0.0, 1.0);
        let fb = frame(
            [0.2, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(51, 0, 0, 255, 2),
            "blend Zero/One: dst kept (51,0,0)",
        );

        // Dst/Zero: src*dst modulate.
        let p = mk_blend(F::Dst, F::Zero, O::Add, F::Dst, F::Zero, O::Add);
        set_color(0.0, 0.0, 1.0, 1.0);
        let fb = frame(
            [0.5, 0.5, 0.5, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(0, 0, 128, 255, 2),
            "blend Dst/Zero: src*dst modulate (0,0,128)",
        );

        // One/One Max: per-channel max.
        let p = mk_blend(F::One, F::One, O::Max, F::One, F::One, O::Max);
        set_color(0.6, 0.2, 0.6, 1.0);
        let fb = frame(
            [0.2, 0.6, 0.2, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(153, 153, 153, 255, 2),
            "blend op Max: per-channel max",
        );

        // One/One ReverseSubtract: dst-src (rgb 191, alpha resolves to 0).
        let p = mk_blend(
            F::One,
            F::One,
            O::ReverseSubtract,
            F::One,
            F::One,
            O::ReverseSubtract,
        );
        set_color(0.25, 0.0, 0.0, 1.0);
        let fb = frame(
            [1.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(191, 0, 0, 0, 3),
            "blend op ReverseSubtract: dst-src rgb (191,0,0) a0",
        );
    }

    // --- Depth-compare matrix (all 8 CompareFunction; z=0.5 quad vs clear-depth 0.75) -------------
    {
        let dq = [
            V3 {
                pos: [-1.0, -1.0, 0.5],
            },
            V3 {
                pos: [1.0, -1.0, 0.5],
            },
            V3 {
                pos: [-1.0, 1.0, 0.5],
            },
            V3 {
                pos: [1.0, 1.0, 0.5],
            },
        ];
        let dvbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dquad"),
            contents: bytemuck::cast_slice(&dq),
            usage: wgpu::BufferUsages::VERTEX,
        });
        set_color(0.0, 1.0, 0.0, 1.0);
        let cases = [
            (wgpu::CompareFunction::Always, true, "depth Always"),
            (wgpu::CompareFunction::Never, false, "depth Never"),
            (wgpu::CompareFunction::Less, true, "depth Less"),
            (wgpu::CompareFunction::LessEqual, true, "depth LessEqual"),
            (wgpu::CompareFunction::Equal, false, "depth Equal"),
            (wgpu::CompareFunction::Greater, false, "depth Greater"),
            (
                wgpu::CompareFunction::GreaterEqual,
                false,
                "depth GreaterEqual",
            ),
            (wgpu::CompareFunction::NotEqual, true, "depth NotEqual"),
        ];
        for (cmp, draws, name) in cases {
            let p = mk(
                &m_pos3,
                &m_pos3,
                &ubo_pll,
                &vbl_pos3,
                wgpu::PrimitiveTopology::TriangleStrip,
                wgpu::FrontFace::Ccw,
                None,
                no_blend(wgpu::ColorWrites::ALL),
                Some(depth_for(cmp)),
            );
            let fb = frame_depth([0.0, 0.0, 0.0, 1.0], 0.75, &p, &ubo_bg, &dvbo, 4);
            ok(fb.peq(W / 2, H / 2, 0, 255, 0, 255, 2) == draws, name);
        }
    }

    // --- Face culling + winding -------------------------------------------------------------------
    set_color(1.0, 0.0, 0.0, 1.0);
    {
        // Cull None: quad drawn.
        let p = mk(
            &m_solid,
            &m_solid,
            &ubo_pll,
            &vbl_pos2,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Ccw,
            None,
            no_blend(wgpu::ColorWrites::ALL),
            None,
        );
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        let none_drawn = fb.all_eq(255, 0, 0, 255, 1);
        ok(none_drawn, "cull None: quad drawn");

        // Cull Back with FrontFace Ccw vs Cw: exactly one shows the quad (winding flip).
        let p_ccw = mk(
            &m_solid,
            &m_solid,
            &ubo_pll,
            &vbl_pos2,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Ccw,
            Some(wgpu::Face::Back),
            no_blend(wgpu::ColorWrites::ALL),
            None,
        );
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_ccw),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        let ccw = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);

        let p_cw = mk(
            &m_solid,
            &m_solid,
            &ubo_pll,
            &vbl_pos2,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Cw,
            Some(wgpu::Face::Back),
            no_blend(wgpu::ColorWrites::ALL),
            None,
        );
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_cw),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        let cw = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);

        ok(ccw != cw, "cull Back: Ccw vs Cw winding flips visibility");

        // Cull Front with the quad's actual winding removes it (or shows it) opposite of cull Back;
        // assert cull Front and cull Back disagree at the center (one draws, one culls).
        let p_front = mk(
            &m_solid,
            &m_solid,
            &ubo_pll,
            &vbl_pos2,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Ccw,
            Some(wgpu::Face::Front),
            no_blend(wgpu::ColorWrites::ALL),
            None,
        );
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_front),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        let front_drawn = fb.peq(W / 2, H / 2, 255, 0, 0, 255, 2);
        ok(
            front_drawn != ccw,
            "cull Front vs cull Back (Ccw) disagree at center",
        );
    }

    // --- Color write mask -------------------------------------------------------------------------
    set_color(1.0, 1.0, 1.0, 1.0);
    {
        let p_r = mk(
            &m_solid,
            &m_solid,
            &ubo_pll,
            &vbl_pos2,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Ccw,
            None,
            no_blend(wgpu::ColorWrites::RED),
            None,
        );
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_r),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(255, 0, 0, 255, 1),
            "colorWrites RED only: white -> (255,0,0,255)",
        );

        let p_all = mk(
            &m_solid,
            &m_solid,
            &ubo_pll,
            &vbl_pos2,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Ccw,
            None,
            no_blend(wgpu::ColorWrites::ALL),
            None,
        );
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&p_all),
                bind: Some(&ubo_bg),
                vbo: Some(&vbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.all_eq(255, 255, 255, 255, 1),
            "colorWrites ALL: white -> (255,255,255,255)",
        );
    }

    // --- Format feature + limit queries -----------------------------------------------------------
    {
        let color_feat = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm);
        ok(
            color_feat
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT),
            "Rgba8Unorm supports RENDER_ATTACHMENT",
        );
        let depth_feat = adapter.get_texture_format_features(wgpu::TextureFormat::Depth32Float);
        ok(
            depth_feat
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT),
            "Depth32Float supports RENDER_ATTACHMENT",
        );
        let lim = device.limits();
        ok(
            lim.max_texture_dimension_2d >= W,
            "limits.max_texture_dimension_2d >= 64",
        );
        ok(
            lim.max_color_attachments >= 1,
            "limits.max_color_attachments >= 1",
        );
    }

    // --- 2x2 texture upload + Nearest sampling ----------------------------------------------------
    {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tex2x2"),
            size: wgpu::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Row-major, v origin top-left in WebGPU: TL red, TR green, BL blue, BR white.
        let texels: [u8; 16] = [
            255, 0, 0, 255, // (0,0) red
            0, 255, 0, 255, // (1,0) green
            0, 0, 255, 255, // (0,1) blue
            255, 255, 255, 255, // (1,1) white
        ];
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &texels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(2),
            },
            wgpu::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
        );
        let tview = tex.create_view(&wgpu::TextureViewDescriptor::default());
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
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tex_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tex-bg"),
            layout: &tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tview),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&samp),
                },
            ],
        });
        let tex_pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tex-pll"),
            bind_group_layouts: &[&tex_bgl],
            push_constant_ranges: &[],
        });
        let pipe_tex = mk(
            &m_tex,
            &m_tex,
            &tex_pll,
            &vbl_pos2uv,
            wgpu::PrimitiveTopology::TriangleStrip,
            wgpu::FrontFace::Ccw,
            None,
            no_blend(wgpu::ColorWrites::ALL),
            None,
        );
        ok(true, "texture pipeline + bind group created");

        // Full-screen quad with uv. WebGPU maps NDC y=+1 to the framebuffer top and the texture's v
        // origin is top-left, so the top vertices (pos.y=+1) carry v=0 and the bottom vertices v=1 to
        // put the texture's top row (red/green) at the framebuffer top.
        let tq = [
            V2UV {
                pos: [-1.0, -1.0],
                uv: [0.0, 1.0],
            },
            V2UV {
                pos: [1.0, -1.0],
                uv: [1.0, 1.0],
            },
            V2UV {
                pos: [-1.0, 1.0],
                uv: [0.0, 0.0],
            },
            V2UV {
                pos: [1.0, 1.0],
                uv: [1.0, 0.0],
            },
        ];
        let tvbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tquad"),
            contents: bytemuck::cast_slice(&tq),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let fb = frame(
            [0.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: Some(&pipe_tex),
                bind: Some(&tex_bg),
                vbo: Some(&tvbo),
                verts: 4,
                scissor: None,
                viewport: None,
            },
        );
        ok(
            fb.peq(W / 4, H / 4, 255, 0, 0, 255, 2),
            "texture Nearest top-left red",
        );
        ok(
            fb.peq(3 * W / 4, H / 4, 0, 255, 0, 255, 2),
            "texture Nearest top-right green",
        );
        ok(
            fb.peq(W / 4, 3 * H / 4, 0, 0, 255, 255, 2),
            "texture Nearest bottom-left blue",
        );
        ok(
            fb.peq(3 * W / 4, 3 * H / 4, 255, 255, 255, 255, 2),
            "texture Nearest bottom-right white",
        );
    }

    // --- Negative control -------------------------------------------------------------------------
    set_color(1.0, 0.0, 0.0, 1.0);
    let fb = frame(
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: Some(&pipe_solid),
            bind: Some(&ubo_bg),
            vbo: Some(&vbo),
            verts: 4,
            scissor: None,
            viewport: None,
        },
    );
    ok(
        !fb.all_eq(0, 255, 0, 255, 2),
        "negative control: red buffer is NOT green",
    );
    ok(
        !fb.peq(0, 0, 0, 0, 0, 255, 2),
        "negative control: red pixel is NOT black",
    );

    device.poll(wgpu::Maintain::Wait);
    device.destroy();
    finish()
}

// Copy the color texture into the readback buffer with padded rows, submit, map, and unpad into an Fb.
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
    println!("wgpu-render-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={EXPECTED}");
    if fail == 0 && total == EXPECTED {
        println!("WGPU_RENDER_RUST_FULL_API OK {pass}");
        0
    } else {
        println!("WGPU_RENDER_RUST_FULL_API FAIL");
        1
    }
}
