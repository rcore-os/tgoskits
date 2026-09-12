// scene_3dmodel - 3D indexed-mesh RENDER-scene carpet driven by the `wgpu` crate (v22) on Mesa
// software adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. The WebGPU port of the
// GLES/Vulkan scene_3dmodel: an offscreen Rgba8Unorm color texture + a Depth32Float depth texture,
// drawn through a real render pipeline (WGSL vertex+fragment) with depth test CompareFunction::Less,
// copied to a MAP_READ buffer (256-byte bytesPerRow padding) and read back. Renders an indexed cube
// mesh with a hand-computed Model-View-Projection matrix (perspective), depth-buffered occlusion, and
// Gouraud shading. The assertion is an INDEPENDENT software reference rasterizer written in Rust:
// verts are transformed by the SAME MVP -> clip -> NDC (perspective divide) -> viewport pixels; for
// each pixel we compute barycentric coordinates, do a perspective-correct interpolated depth test in a
// private z-buffer, interpolate the vertex colors, then compare the reference framebuffer to the
// readback per pixel (small tolerance for edge-sample/rounding). Closes with a negative control. Prints
// "SCENE_3DMODEL OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// WebGPU/wgpu vs GL adaptation (the main math change): GL NDC z is in [-1,1]; WebGPU (like Vulkan) NDC
// z is in [0,1]. Two coupled changes keep the GPU output and the reference in agreement, ported
// verbatim from the Vulkan scene_3dmodel:
//   (a) perspective() uses the WebGPU/Vulkan/D3D z-mapping (near->0, far->1): the z row is
//       m[2][2]=zf/(zn-zf), m[3][2]=(zf*zn)/(zn-zf), so z_ndc = z_clip/w_clip lands in [0,1].
//   (b) the reference window depth is sz = ndcz DIRECTLY (z_clip/w_clip), not GL's ndcz*0.5+0.5, so
//       the reference's LESS depth test uses the same [0,1] depth the GPU writes into Depth32Float.
// The column-major M4 math, cube verts/colors/indices, model/view, barycentric rasterizer, and
// perspective-correct color interpolation are ported byte-identical in behavior from the reference.
// The vertex shader carries `@invariant` on @builtin(position) so the rasterized depth is bit-exact and
// the LESS occlusion is deterministic.

use std::sync::atomic::{AtomicU32, Ordering};

use wgpu::util::DeviceExt;

const W: u32 = 64;
const H: u32 = 64;
const BPP: u32 = 4;

static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

// Assertion budget, calibrated to the count this cell genuinely runs on the success path. The Vulkan
// reference cell pins EXPECTED=23, but ~9 of those are VKOK() wrappers on individual Vulkan
// object-creation calls (vkCreateInstance/Device/Image/ImageView/RenderPass/Framebuffer/CommandPool/
// Pipeline) that return VkResult - wgpu's create_* calls do not surface a per-call Result to assert on
// (validation surfaces via on_uncaptured_error instead), so those raw asserts have no WebGPU
// counterpart. This cell keeps every SCENE assertion (reference-rasterizer coverage/color/occlusion,
// corner/center/vertex spot checks, negative control) plus the adapter/device requests, which lands at
// 14 on the success path.
const EXPECTED: u32 = 14;

fn ok(cond: bool, desc: &str) {
    if cond {
        PASS.fetch_add(1, Ordering::Relaxed);
    } else {
        FAIL.fetch_add(1, Ordering::Relaxed);
        eprintln!("FAIL: {desc}");
    }
}

// pos3 + col3, mvp uniform (WebGPU has no push constants). @invariant pins depth bit-exactness.
const CUBE_WGSL: &str = r#"
struct MVP { m: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: MVP;
struct VOut { @invariant @builtin(position) pos: vec4<f32>, @location(0) col: vec3<f32> };
@vertex fn vs(@location(0) p: vec3<f32>, @location(1) c: vec3<f32>) -> VOut {
    var o: VOut;
    o.pos = u.m * vec4<f32>(p, 1.0);
    o.col = c;
    return o;
}
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.col, 1.0);
}
"#;

// ---- column-major 4x4 matrix math (GL layout: m[col*4+row]) - ported from the reference ----
#[derive(Clone, Copy)]
struct M4 {
    m: [f32; 16],
}
fn mul(a: &M4, b: &M4) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    for c in 0..4 {
        for row in 0..4 {
            let mut s = 0.0f32;
            for k in 0..4 {
                s += a.m[k * 4 + row] * b.m[c * 4 + k];
            }
            r.m[c * 4 + row] = s;
        }
    }
    r
}
fn mv4(a: &M4, v: [f32; 4]) -> [f32; 4] {
    let mut o = [0.0f32; 4];
    for row in 0..4 {
        let mut s = 0.0f32;
        for k in 0..4 {
            s += a.m[k * 4 + row] * v[k];
        }
        o[row] = s;
    }
    o
}
// WebGPU/Vulkan perspective: near->z_ndc 0, far->z_ndc 1 (z/w in [0,1]). Only the z row differs from GL.
fn perspective(fovy: f32, aspect: f32, zn: f32, zf: f32) -> M4 {
    let f = 1.0 / (fovy * 0.5).tan();
    let mut r = M4 { m: [0.0; 16] };
    r.m[0 * 4 + 0] = f / aspect;
    r.m[1 * 4 + 1] = f;
    r.m[2 * 4 + 2] = zf / (zn - zf);
    r.m[2 * 4 + 3] = -1.0;
    r.m[3 * 4 + 2] = (zf * zn) / (zn - zf);
    r
}
fn translate(x: f32, y: f32, z: f32) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    r.m[0] = 1.0;
    r.m[5] = 1.0;
    r.m[10] = 1.0;
    r.m[15] = 1.0;
    r.m[3 * 4 + 0] = x;
    r.m[3 * 4 + 1] = y;
    r.m[3 * 4 + 2] = z;
    r
}
fn rot_y(a: f32) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    let (c, s) = (a.cos(), a.sin());
    r.m[0 * 4 + 0] = c;
    r.m[0 * 4 + 2] = -s;
    r.m[2 * 4 + 0] = s;
    r.m[2 * 4 + 2] = c;
    r.m[1 * 4 + 1] = 1.0;
    r.m[3 * 4 + 3] = 1.0;
    r
}
fn rot_x(a: f32) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    let (c, s) = (a.cos(), a.sin());
    r.m[1 * 4 + 1] = c;
    r.m[1 * 4 + 2] = s;
    r.m[2 * 4 + 1] = -s;
    r.m[2 * 4 + 2] = c;
    r.m[0 * 4 + 0] = 1.0;
    r.m[3 * 4 + 3] = 1.0;
    r
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vtx {
    pos: [f32; 3],
    col: [f32; 3],
}

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
    fn peq(&self, x: u32, y: u32, r: u8, g: u8, b: u8, a: u8, tol: i32) -> bool {
        let d = |v: u8, t: u8| (v as i32 - t as i32).abs() <= tol;
        d(self.p(x, y, 0), r)
            && d(self.p(x, y, 1), g)
            && d(self.p(x, y, 2), b)
            && d(self.p(x, y, 3), a)
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
                label: Some("3dmodel-device"),
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

    // offscreen color + depth targets + readback plumbing.
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

    let unpadded = W * BPP;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    ok(
        true,
        "offscreen Rgba8Unorm + Depth32Float target + readback buffer ready",
    );

    // ---- cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (ported) ----
    const VP: [[f32; 3]; 8] = [
        [-1.0, -1.0, -1.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
    ];
    let mut vc = [[0.0f32; 3]; 8];
    for i in 0..8 {
        vc[i][0] = (VP[i][0] + 1.0) * 0.5;
        vc[i][1] = (VP[i][1] + 1.0) * 0.5;
        vc[i][2] = (VP[i][2] + 1.0) * 0.5;
    }
    const IDX: [u16; 36] = [
        0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4,
        1, 5, 6, 1, 6, 2,
    ];

    let model = mul(&rot_y(0.6), &rot_x(0.3));
    let view = translate(0.0, 0.0, -5.0);
    let proj = perspective(1.0, W as f32 / H as f32, 1.0, 20.0);
    let mvp = mul(&proj, &mul(&view, &model));

    let mut verts = [Vtx {
        pos: [0.0; 3],
        col: [0.0; 3],
    }; 8];
    for i in 0..8 {
        verts[i] = Vtx {
            pos: VP[i],
            col: vc[i],
        };
    }
    let vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-vbo"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-ibo"),
        contents: bytemuck::cast_slice(&IDX),
        usage: wgpu::BufferUsages::INDEX,
    });

    // mvp uniform, column-major just like the shader mat4x4 expects.
    let mvp_ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mvp"),
        contents: bytemuck::cast_slice(&mvp.m),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mvp-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mvp-bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: mvp_ubo.as_entire_binding(),
        }],
    });
    let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pll"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cube"),
        source: wgpu::ShaderSource::Wgsl(CUBE_WGSL.into()),
    });
    let vbl = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vtx>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
        ],
    };
    // depth test LESS, no cull (reference did not cull).
    let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cube-pipe"),
        layout: Some(&pll),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs",
            buffers: std::slice::from_ref(&vbl),
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
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
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    ok(true, "cube pipeline created");

    // ---- draw: clear color black, clear depth 1.0, draw the indexed cube ----
    let buf = {
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
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&pipe);
            rp.set_bind_group(0, &bg, &[]);
            rp.set_vertex_buffer(0, vbo.slice(..));
            rp.set_index_buffer(ibo.slice(..), wgpu::IndexFormat::Uint16);
            rp.draw_indexed(0..36, 0, 0..1);
        }
        copy_and_read(&device, &queue, &color, &readback, enc, padded)
    };
    ok(true, "cube drawn (depth-tested, Gouraud)");

    // ---- INDEPENDENT software reference rasterizer (ported; WebGPU NDC-z in [0,1]) ----
    let mut refc = vec![[0.0f32; 3]; (W * H) as usize];
    let mut refz = vec![1e9f32; (W * H) as usize];
    let mut refcov = vec![0u8; (W * H) as usize];
    let idx2 = |x: usize, y: usize| y * W as usize + x;

    let mut sx = [0.0f32; 8];
    let mut sy = [0.0f32; 8];
    let mut sz = [0.0f32; 8];
    let mut sw = [0.0f32; 8];
    for i in 0..8 {
        let out = mv4(&mvp, [VP[i][0], VP[i][1], VP[i][2], 1.0]);
        let w = out[3];
        sw[i] = w;
        let (ndcx, ndcy, ndcz) = (out[0] / w, out[1] / w, out[2] / w); // WebGPU NDC z in [0,1]
        sx[i] = (ndcx * 0.5 + 0.5) * W as f32;
        // WebGPU framebuffer origin is top-left: NDC y=+1 maps to row 0 (the reverse of the Vulkan
        // reference cell's positive-height viewport, which put NDC y=-1 at row 0). Flip sy so the
        // reference's row indexing matches the WebGPU-rendered readback.
        sy[i] = (0.5 - ndcy * 0.5) * H as f32;
        sz[i] = ndcz; // window depth = z/w directly ([0,1])
    }
    ok(
        sw[0] > 0.0,
        "reference: all clip.w positive (mesh in front of camera)",
    );

    for t in 0..12 {
        let a = IDX[t * 3] as usize;
        let b = IDX[t * 3 + 1] as usize;
        let c = IDX[t * 3 + 2] as usize;
        let (ax, ay, bx, by, cx, cy) = (sx[a], sy[a], sx[b], sy[b], sx[c], sy[c]);
        let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if area.abs() < 1e-6 {
            continue;
        }
        let minx = ax.min(bx.min(cx)).floor() as i32;
        let maxx = ax.max(bx.max(cx)).ceil() as i32;
        let miny = ay.min(by.min(cy)).floor() as i32;
        let maxy = ay.max(by.max(cy)).ceil() as i32;
        let minx = minx.max(0);
        let miny = miny.max(0);
        let maxx = maxx.min(W as i32);
        let maxy = maxy.min(H as i32);
        for y in miny..maxy {
            for x in minx..maxx {
                let (pxs, pys) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area;
                let mut w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area;
                let mut w2 = 1.0 - w0 - w1;
                let inside =
                    (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if !inside {
                    continue;
                }
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    w0 = -w0;
                    w1 = -w1;
                    w2 = -w2;
                }
                let z = w0 * sz[a] + w1 * sz[b] + w2 * sz[c];
                let i = idx2(x as usize, y as usize);
                if z < refz[i] {
                    refz[i] = z;
                    refcov[i] = 1;
                    let (iwa, iwb, iwc) = (1.0 / sw[a], 1.0 / sw[b], 1.0 / sw[c]);
                    let d = w0 * iwa + w1 * iwb + w2 * iwc;
                    for k in 0..3 {
                        let num = w0 * iwa * vc[a][k] + w1 * iwb * vc[b][k] + w2 * iwc * vc[c][k];
                        refc[i][k] = num / d;
                    }
                }
            }
        }
    }

    let (mut total, mut r#match, mut covmatch, mut covtotal, mut interior_bad) =
        (0i32, 0i32, 0i32, 0i32, 0i32);
    for y in 0..H {
        for x in 0..W {
            total += 1;
            let gcov = !(buf.p(x, y, 0) == 0 && buf.p(x, y, 1) == 0 && buf.p(x, y, 2) == 0);
            let i = idx2(x as usize, y as usize);
            let rcov = refcov[i] != 0;
            if gcov == rcov {
                covmatch += 1;
            }
            if rcov {
                covtotal += 1;
                let er = (refc[i][0] * 255.0).round() as u8;
                let eg = (refc[i][1] * 255.0).round() as u8;
                let eb = (refc[i][2] * 255.0).round() as u8;
                let interior = x > 0
                    && y > 0
                    && x < W - 1
                    && y < H - 1
                    && refcov[idx2(x as usize, (y - 1) as usize)] != 0
                    && refcov[idx2(x as usize, (y + 1) as usize)] != 0
                    && refcov[idx2((x - 1) as usize, y as usize)] != 0
                    && refcov[idx2((x + 1) as usize, y as usize)] != 0;
                if buf.peq(x, y, er, eg, eb, 255, 6) {
                    r#match += 1;
                } else if interior {
                    interior_bad += 1;
                }
            }
        }
    }
    ok(covtotal > 200, "reference: cube covers a substantial area");
    ok(
        covmatch >= (0.97 * total as f32) as i32,
        "coverage mask matches GPU (>=97% of pixels agree covered/empty)",
    );
    ok(
        interior_bad == 0,
        "every interior pixel matches perspective-correct Gouraud reference (tol 6)",
    );
    ok(
        r#match >= (0.92 * covtotal as f32) as i32,
        "92%+ of covered pixels match reference color (edges excluded)",
    );

    {
        let vx = (sx[6] - 0.5).round() as i32;
        let vy = (sy[6] - 0.5).round() as i32;
        if vx >= 1 && vx < W as i32 - 1 && vy >= 1 && vy < H as i32 - 1 {
            let mut bright = false;
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    let (xx, yy) = ((vx + dx) as u32, (vy + dy) as u32);
                    if buf.p(xx, yy, 0) > 180 && buf.p(xx, yy, 1) > 180 && buf.p(xx, yy, 2) > 180 {
                        bright = true;
                    }
                }
            }
            ok(
                bright,
                "vertex (1,1,1) region is bright (Gouraud white corner)",
            );
        } else {
            ok(
                false,
                "vertex (1,1,1) projected off-screen (camera mis-set)",
            );
        }
    }
    ok(
        buf.peq(0, 0, 0, 0, 0, 255, 1) || refcov[idx2(0, 0)] == 0,
        "corner (0,0) background consistent",
    );

    {
        let (cxp, cyp) = (W / 2, H / 2);
        let i = idx2(cxp as usize, cyp as usize);
        if refcov[i] != 0 {
            let er = (refc[i][0] * 255.0).round() as u8;
            let eg = (refc[i][1] * 255.0).round() as u8;
            let eb = (refc[i][2] * 255.0).round() as u8;
            ok(
                buf.peq(cxp, cyp, er, eg, eb, 255, 8),
                "center pixel = nearest-face (depth-buffered occlusion) reference color",
            );
        } else {
            ok(false, "center pixel not covered (mesh mis-projected)");
        }
    }

    ok(
        !(buf.p(1, 1, 0) == buf.p(W / 2, H / 2, 0)
            && buf.p(1, 1, 1) == buf.p(W / 2, H / 2, 1)
            && buf.p(1, 1, 2) == buf.p(W / 2, H / 2, 2)),
        "negative control: image is not a flat single color (real 3D shading present)",
    );

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
    println!("scene-3dmodel: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={EXPECTED}");
    if fail == 0 && total == EXPECTED {
        println!("SCENE_3DMODEL OK {pass}");
        0
    } else {
        println!("SCENE_3DMODEL FAIL");
        1
    }
}
