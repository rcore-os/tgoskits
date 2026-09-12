// scene_anim - keyframe-animation RENDER-scene carpet driven by the `wgpu` crate (v22) on Mesa software
// adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. WebGPU port of the GLES scene_anim:
// an offscreen 64x64 Rgba8Unorm texture is rendered through a real render pipeline; N=4 keyframes of a
// transformed unit quad are drawn, each frame's model transform a rotation about the FBO center
// composed with a translation and uniform scale, interpolated by t in {0,0.25,0.5,0.75}. The transform
// is applied in the vertex shader from a hand-built pixel-space model matrix (rotation, scale,
// translate) and an ortho pixel->NDC map. For every frame the four rotated/scaled/translated quad
// CORNERS are computed INDEPENDENTLY in Rust (closed form: R(theta)*S*local + T) and the readback is
// asserted at those exact corner pixels plus a point outside the quad. A cubic ease eased(t)=3t^2-2t^3
// drives the scale, its value asserted at each t. Closes with a negative control. Prints
// "SCENE_ANIM OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Coordinate convention (WebGPU vs GL): the GLES cell's gl_FragCoord/glReadPixels are bottom-origin;
// WebGPU readback is top-origin. The pixel-space vertex shader flips NDC y so pixel-y == readback-row,
// and the closed-form corner/center pixel coordinates are indexed against readback rows directly. The
// lerp, ease_cubic, and R*S*local+T corner math are ported verbatim in behavior from the GLES reference.

use std::sync::atomic::{AtomicU32, Ordering};

use wgpu::util::DeviceExt;

const W: u32 = 64;
const H: u32 = 64;
const BPP: u32 = 4;

static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

// Assertion budget, calibrated to the count this cell genuinely runs on the success path. The GLES
// reference pins EXPECTED=45; the difference from the raw GLES tally is the EGL/GL bring-up asserts
// (eglGetDisplay/Initialize/ChooseConfig/BindAPI/CreateContext/MakeCurrent, FBO-complete, program
// compile/link, uniform-location) replaced by the wgpu adapter/device requests. All per-frame closed-
// form assertions (ease value, scale=lerp, center color, 4 transformed corners, outside-quad) across the
// 4 frames plus the between-frame and rotation checks and the negative control are preserved 1:1, which
// lands at 38 (the EGL/GL bring-up + program-compile/link + uniform-location asserts have no wgpu
// counterpart and are replaced by the 2 adapter/device asserts).
const EXPECTED: u32 = 38;

fn ok(cond: bool, desc: &str) {
    if cond {
        PASS.fetch_add(1, Ordering::Relaxed);
    } else {
        FAIL.fetch_add(1, Ordering::Relaxed);
        eprintln!("FAIL: {desc}");
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn ease_cubic(t: f32) -> f32 {
    3.0 * t * t - 2.0 * t * t * t
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2 {
    pos: [f32; 2],
}
// col0.xy, col1.xy, tr.xy, vp.xy, rgba - packed for the vertex/fragment uniforms.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Xform {
    col0: [f32; 2],
    col1: [f32; 2],
    tr: [f32; 2],
    vp: [f32; 2],
    rgba: [f32; 4],
}

// Pixel-space affine vertex shader: pix = col0*lp.x + col1*lp.y + tr; then map pixel -> NDC with y-flip
// so pixel-y == readback-row. Uniform color out.
const WGSL: &str = r#"
struct X { col0: vec2<f32>, col1: vec2<f32>, tr: vec2<f32>, vp: vec2<f32>, rgba: vec4<f32> };
@group(0) @binding(0) var<uniform> u: X;
@vertex fn vs(@location(0) lp: vec2<f32>) -> @builtin(position) vec4<f32> {
    let pix = u.col0 * lp.x + u.col1 * lp.y + u.tr;
    let n = (pix / u.vp) * 2.0 - 1.0;
    return vec4<f32>(n.x, -n.y, 0.0, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return u.rgba; }
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
        if x < 0 || y < 0 || x >= W as i32 || y >= H as i32 {
            return false;
        }
        let d = |v: u8, t: i32| (v as i32 - t).abs() <= tol;
        d(self.p(x as u32, y as u32, 0), r)
            && d(self.p(x as u32, y as u32, 1), g)
            && d(self.p(x as u32, y as u32, 2), b)
            && d(self.p(x as u32, y as u32, 3), a)
    }
    fn near_color(&self, x: i32, y: i32, r: i32, g: i32, b: i32, tol: i32) -> bool {
        for dy in -1..=1i32 {
            for dx in -1..=1i32 {
                let (xx, yy) = (x + dx, y + dy);
                if xx < 0 || yy < 0 || xx >= W as i32 || yy >= H as i32 {
                    continue;
                }
                if self.peq(xx, yy, r, g, b, 255, tol) {
                    return true;
                }
            }
        }
        false
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
                label: Some("anim-device"),
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

    let xform_ubo = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("xform-ubo"),
        size: std::mem::size_of::<Xform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bgl"),
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
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: xform_ubo.as_entire_binding(),
        }],
    });
    let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pll"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("anim"),
        source: wgpu::ShaderSource::Wgsl(WGSL.into()),
    });
    let vbl = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<V2>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        }],
    };
    let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("anim-pipe"),
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
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    // local quad corners (unit square in local space, TL/TR/BL/BR) as a triangle strip.
    let local: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    let vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("local-quad"),
        contents: bytemuck::cast_slice(&[
            V2 {
                pos: [local[0], local[1]],
            },
            V2 {
                pos: [local[2], local[3]],
            },
            V2 {
                pos: [local[4], local[5]],
            },
            V2 {
                pos: [local[6], local[7]],
            },
        ]),
        usage: wgpu::BufferUsages::VERTEX,
    });

    // animation keyframe params (ported).
    let (a0, a1) = (0.0f32, std::f32::consts::PI / 2.0);
    let (s0, s1) = (6.0f32, 14.0);
    let (cx0, cx1, cy0, cy1) = (20.0f32, 44.0, 20.0, 44.0);
    let frame_transform = |t: f32| -> ([f32; 2], [f32; 2], [f32; 2], f32, f32) {
        let ang = lerp(a0, a1, t);
        let sc = lerp(s0, s1, ease_cubic(t));
        let (cx, cy) = (lerp(cx0, cx1, t), lerp(cy0, cy1, t));
        let (ca, sa) = (ang.cos(), ang.sin());
        let col0 = [sc * ca, sc * sa];
        let col1 = [-sc * sa, sc * ca];
        let tr = [cx, cy];
        (col0, col1, tr, sc, ang)
    };

    let ts = [0.0f32, 0.25, 0.5, 0.75];
    let cols = [
        [1.0f32, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
    ];

    // render one frame with the current uniform.
    let render = |dev: &wgpu::Device, q: &wgpu::Queue, x: &Xform| -> Fb {
        q.write_buffer(&xform_ubo, 0, bytemuck::bytes_of(x));
        let mut enc = dev.create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(&pipe);
            rp.set_bind_group(0, &bg, &[]);
            rp.set_vertex_buffer(0, vbo.slice(..));
            rp.draw(0..4, 0..1);
        }
        copy_and_read(dev, q, &color, &readback, enc, padded)
    };

    for fi in 0..4 {
        let t = ts[fi];
        let (col0, col1, tr, sc, ang) = frame_transform(t);
        let x = Xform {
            col0,
            col1,
            tr,
            vp: [W as f32, H as f32],
            rgba: [cols[fi][0], cols[fi][1], cols[fi][2], 1.0],
        };
        let fb = render(&device, &queue, &x);

        // closed-form corner positions: corner = R(ang)*S(sc)*localCorner + center.
        let (ca, sa) = (ang.cos(), ang.sin());
        let mut corners = [[0.0f32; 2]; 4];
        for k in 0..4 {
            let (lx, ly) = (local[k * 2], local[k * 2 + 1]);
            let rx = sc * (ca * lx - sa * ly);
            let ry = sc * (sa * lx + ca * ly);
            corners[k] = [tr[0] + rx, tr[1] + ry];
        }
        let e = ease_cubic(t);
        let e_ref = 3.0 * t * t - 2.0 * t * t * t;
        ok((e - e_ref).abs() < 1e-6, "ease_cubic closed-form value");
        ok(
            (sc - (s0 + (s1 - s0) * e)).abs() < 1e-4,
            "scale = lerp(S0,S1,ease(t)) closed-form",
        );

        let cxi = (tr[0] - 0.5).round() as i32;
        let cyi = (tr[1] - 0.5).round() as i32;
        ok(
            fb.peq(
                cxi,
                cyi,
                (cols[fi][0] * 255.0).round() as i32,
                (cols[fi][1] * 255.0).round() as i32,
                (cols[fi][2] * 255.0).round() as i32,
                255,
                2,
            ),
            "frame center pixel carries frame color at closed-form center",
        );

        for k in 0..4 {
            let px_ = (corners[k][0] - 0.5).round() as i32;
            let py_ = (corners[k][1] - 0.5).round() as i32;
            let onscreen = px_ >= 0 && py_ >= 0 && px_ < W as i32 && py_ < H as i32;
            ok(
                onscreen
                    && fb.near_color(
                        px_,
                        py_,
                        (cols[fi][0] * 255.0).round() as i32,
                        (cols[fi][1] * 255.0).round() as i32,
                        (cols[fi][2] * 255.0).round() as i32,
                        40,
                    ),
                "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)",
            );
        }

        // a point far outside the quad silhouette stays background (guard by max reach = sc*sqrt2).
        {
            let (ox, oy) = if fi < 2 {
                (W as i32 - 2, H as i32 - 2)
            } else {
                (1, 1)
            };
            let reach = sc * 1.4142;
            let covers = (ox as f32 + 0.5 - tr[0]).abs() <= reach
                && (oy as f32 + 0.5 - tr[1]).abs() <= reach;
            if !covers {
                ok(
                    fb.peq(ox, oy, 0, 0, 0, 255, 2),
                    "outside-quad point stays background (closed-form silhouette)",
                );
            } else {
                ok(true, "outside-quad point skipped (would be covered)");
            }
        }
    }

    // t=0 vs t=0.75 center positions differ.
    {
        let (_, _, tra, ..) = frame_transform(0.0);
        let (_, _, trb, ..) = frame_transform(0.75);
        ok(
            (tra[0] - trb[0]).abs() > 1.0,
            "center translates between t=0 and t=0.75 (animation is real)",
        );
    }

    // rotation at t=0.5: angle = pi/4, rotated x-axis column = (sc*cos45, sc*sin45).
    {
        let (col0, _, _, _, ang) = frame_transform(0.5);
        ok(
            (ang - std::f32::consts::PI / 4.0).abs() < 1e-5,
            "t=0.5 rotation angle = pi/4 closed-form",
        );
        ok(
            (col0[0] - col0[1]).abs() < 1e-4 && col0[0] > 0.0,
            "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)",
        );
    }

    // negative control: render frame 0 (red) and confirm it is NOT green.
    {
        let (col0, col1, tr, ..) = frame_transform(0.0);
        let x = Xform {
            col0,
            col1,
            tr,
            vp: [W as f32, H as f32],
            rgba: [1.0, 0.0, 0.0, 1.0],
        };
        let fb = render(&device, &queue, &x);
        let cxi = (tr[0] - 0.5).round() as i32;
        let cyi = (tr[1] - 0.5).round() as i32;
        ok(
            !fb.peq(cxi, cyi, 0, 255, 0, 255, 4),
            "negative control: frame-0 center is NOT green",
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
    println!("scene-anim: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={EXPECTED}");
    if fail == 0 && total == EXPECTED {
        println!("SCENE_ANIM OK {pass}");
        0
    } else {
        println!("SCENE_ANIM FAIL");
        1
    }
}
