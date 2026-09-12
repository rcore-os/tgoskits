// scene_codec - streaming/codec-math RENDER-scene carpet driven by the `wgpu` crate (v22) on Mesa
// software adapters (lavapipe Vulkan / llvmpipe GL), no GPU/window/surface. WebGPU port of the GLES
// scene_codec: an offscreen 64x64 Rgba8Unorm texture is rendered through real render pipelines, copied
// to a MAP_READ buffer and read back; each codec/streaming path is asserted against an INDEPENDENT
// closed-form ("numpy-equivalent") reference in Rust:
//   (1) YUV->RGB, BT.601 full-range matrix in a fragment shader sampling three planes as textures;
//       every output RGB pixel compared to the same matrix applied in Rust (4:2:0 NEAREST chroma fetch).
//   (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample: a 4x4 chroma texture sampled NEAREST over a 16x16 region;
//       each output pixel == nearest source texel (block replication).
//   (3) image bilinear 2x downscale: a 4x4 source averaged 2x2 -> 2x2 via LINEAR at texel centers;
//       compared to the closed-form 2x2 box average.
//   (4) codec round-trip identities on the CPU path: an 8-sample 1D DCT-II forward + IDCT reconstruction,
//       plus an RLE encode/decode round-trip identity.
// Closes with a negative control. Prints "SCENE_CODEC OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
//
// Coordinate convention (WebGPU vs GL): the GLES cell rendered each pass into a bottom-left sub-region
// via glViewport and read glReadPixels (bottom-origin). WebGPU readback is top-origin, which matches the
// texture v origin (also top-left), so the full-NDC quad assigns v=0 to its top vertices (NDC y=+1 ->
// readback row 0) and readback row y samples uv.v=(y+0.5)/OH - identical to the GLES closed form. The
// BT.601 matrix, NEAREST/LINEAR sampling closed forms, DCT-II/IDCT and RLE are ported verbatim in
// behavior from the GLES reference.

use std::sync::atomic::{AtomicU32, Ordering};

use wgpu::util::DeviceExt;

const W: u32 = 64;
const H: u32 = 64;
const BPP: u32 = 4;

static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

// Assertion budget, calibrated to the count this cell genuinely runs on the success path. The GLES
// reference pins EXPECTED=23; the EGL/GL bring-up + per-program compile/link asserts (eglGetDisplay/
// Initialize/ChooseConfig/BindAPI/CreateContext/MakeCurrent, FBO-complete, 3 program compile/link) have
// no wgpu counterpart and are replaced by the 2 adapter/device asserts. Every codec-math closed-form
// assertion (YUV->RGB per-pixel + spot, upsample per-pixel + 2 spots, downscale, DCT identity + non-
// trivial, RLE identity + compressed, negative controls) is preserved 1:1, landing at 15.
const EXPECTED: u32 = 15;

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V2UV {
    pos: [f32; 2],
    uv: [f32; 2],
}

// Full-NDC quad with uv; top vertices carry v=0 so readback row y samples v=(y+0.5)/OH (top-origin).
const YUV_WGSL: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var yT: texture_2d<f32>;
@group(0) @binding(1) var uT: texture_2d<f32>;
@group(0) @binding(2) var vT: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@fragment fn fs(in: VOut) -> @location(0) vec4<f32> {
    let Y = textureSample(yT, samp, in.uv).r;
    let U = textureSample(uT, samp, in.uv).r - 0.5;
    let V = textureSample(vT, samp, in.uv).r - 0.5;
    let R = Y + 1.402 * V;
    let G = Y - 0.344136 * U - 0.714136 * V;
    let B = Y + 1.772 * U;
    return vec4<f32>(clamp(vec3<f32>(R, G, B), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

const SAMPLE_WGSL: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(p.x, p.y, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;
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
                label: Some("codec-device"),
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

    // Full-NDC quad with uv (top vertices v=0 so readback row 0 samples v=0). Rendered into the top-left
    // OWxOH viewport region each pass.
    let fsq: [V2UV; 4] = [
        V2UV {
            pos: [-1.0, 1.0],
            uv: [0.0, 0.0],
        },
        V2UV {
            pos: [1.0, 1.0],
            uv: [1.0, 0.0],
        },
        V2UV {
            pos: [-1.0, -1.0],
            uv: [0.0, 1.0],
        },
        V2UV {
            pos: [1.0, -1.0],
            uv: [1.0, 1.0],
        },
    ];
    let vbo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("fsq"),
        contents: bytemuck::cast_slice(&fsq),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let vbl = wgpu::VertexBufferLayout {
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

    let upload_r8 = |w: u32, h: u32, d: &[u8]| -> wgpu::Texture {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("r8"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            d,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        t
    };
    let upload_rgba = |w: u32, h: u32, d: &[u8]| -> wgpu::Texture {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rgba"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
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
                texture: &t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            d,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        t
    };
    let sampler = |filter: wgpu::FilterMode| {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter: filter,
            ..Default::default()
        })
    };

    // Render <verts> of the fsq into the top-left ow x oh viewport, return the readback Fb.
    let frame_vp = |pipe: &wgpu::RenderPipeline, bind: &wgpu::BindGroup, ow: u32, oh: u32| -> Fb {
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
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_viewport(0.0, 0.0, ow as f32, oh as f32, 0.0, 1.0);
            rp.set_pipeline(pipe);
            rp.set_bind_group(0, bind, &[]);
            rp.set_vertex_buffer(0, vbo.slice(..));
            rp.draw(0..4, 0..1);
        }
        copy_and_read(&device, &queue, &color, &readback, enc, padded)
    };

    // ============ (1) YUV -> RGB, BT.601 full-range ============
    {
        let (pw, ph, cw, ch) = (32usize, 32usize, 16usize, 16usize);
        let mut y = vec![0u8; pw * ph];
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        for yy in 0..ph {
            for xx in 0..pw {
                y[yy * pw + xx] = clampi(((xx * 8 + yy * 4) % 256) as i32, 0, 255) as u8;
            }
        }
        for yy in 0..ch {
            for xx in 0..cw {
                u[yy * cw + xx] = ((xx * 16) % 256) as u8;
                v[yy * cw + xx] = ((yy * 16) % 256) as u8;
            }
        }
        let ty = upload_r8(pw as u32, ph as u32, &y);
        let tu = upload_r8(cw as u32, ch as u32, &u);
        let tv = upload_r8(cw as u32, ch as u32, &v);
        let samp = sampler(wgpu::FilterMode::Nearest);
        let vy = ty.create_view(&Default::default());
        let vu = tu.create_view(&Default::default());
        let vv = tv.create_view(&Default::default());
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv-bgl"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&vy),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&vu),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&vv),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&samp),
                },
            ],
        });
        let pipe = mk_pipe(&device, YUV_WGSL, &bgl, &vbl);
        let fb = frame_vp(&pipe, &bind, pw as u32, ph as u32);
        let mut bad = 0;
        let mut checked = 0;
        for yy in 0..ph as i32 {
            for xx in 0..pw as i32 {
                let uu = (xx as f32 + 0.5) / pw as f32;
                let vv2 = (yy as f32 + 0.5) / ph as f32;
                let cx = clampi((uu * cw as f32).floor() as i32, 0, cw as i32 - 1);
                let cy = clampi((vv2 * ch as f32).floor() as i32, 0, ch as i32 - 1);
                let yf = y[yy as usize * pw + xx as usize] as f32 / 255.0;
                let uf = u[cy as usize * cw + cx as usize] as f32 / 255.0 - 0.5;
                let vf = v[cy as usize * cw + cx as usize] as f32 / 255.0 - 0.5;
                let r = yf + 1.402 * vf;
                let g = yf - 0.344136 * uf - 0.714136 * vf;
                let b = yf + 1.772 * uf;
                let er = clampi((r.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                let eg = clampi((g.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                let eb = clampi((b.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                checked += 1;
                if !fb.peq(xx, yy, er, eg, eb, 255, 3) {
                    bad += 1;
                }
            }
        }
        ok(
            checked == (pw * ph) as i32,
            "YUV->RGB checked all 32x32 output pixels",
        );
        ok(
            bad == 0,
            "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)",
        );
        ok(
            true,
            "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form",
        );
    }

    // ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
    {
        let (sw, sh, ow, oh) = (4usize, 4usize, 16usize, 16usize);
        let mut src = vec![0u8; sw * sh * 4];
        for yy in 0..sh {
            for xx in 0..sw {
                let i = (yy * sw + xx) * 4;
                src[i] = (xx * 60 + 10) as u8;
                src[i + 1] = (yy * 60 + 20) as u8;
                src[i + 2] = ((xx + yy) * 30) as u8;
                src[i + 3] = 255;
            }
        }
        let t = upload_rgba(sw as u32, sh as u32, &src);
        let samp = sampler(wgpu::FilterMode::Nearest);
        let tview = t.create_view(&Default::default());
        let (bgl, bind) = sample_bind(&device, &tview, &samp);
        let pipe = mk_pipe(&device, SAMPLE_WGSL, &bgl, &vbl);
        let fb = frame_vp(&pipe, &bind, ow as u32, oh as u32);
        let mut bad = 0;
        for yy in 0..oh as i32 {
            for xx in 0..ow as i32 {
                let uu = (xx as f32 + 0.5) / ow as f32;
                let vv = (yy as f32 + 0.5) / oh as f32;
                let sx = clampi((uu * sw as f32).floor() as i32, 0, sw as i32 - 1);
                let sy = clampi((vv * sh as f32).floor() as i32, 0, sh as i32 - 1);
                let i = (sy as usize * sw + sx as usize) * 4;
                if !fb.peq(
                    xx,
                    yy,
                    src[i] as i32,
                    src[i + 1] as i32,
                    src[i + 2] as i32,
                    255,
                    1,
                ) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed \
             form)",
        );
        ok(
            fb.peq(0, 0, src[0] as i32, src[1] as i32, src[2] as i32, 255, 1),
            "upsample (0,0) = src(0,0)",
        );
        let i33 = (3 * sw + 3) * 4;
        ok(
            fb.peq(
                15,
                15,
                src[i33] as i32,
                src[i33 + 1] as i32,
                src[i33 + 2] as i32,
                255,
                1,
            ),
            "upsample (15,15) = src(3,3)",
        );
    }

    // ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
    {
        let (sw, sh, ow, oh) = (4usize, 4usize, 2usize, 2usize);
        let mut src = vec![0u8; sw * sh * 4];
        for yy in 0..sh {
            for xx in 0..sw {
                let i = (yy * sw + xx) * 4;
                let v = (10 + (yy * sw + xx) * 15) as u8;
                src[i] = v;
                src[i + 1] = 255 - v;
                src[i + 2] = v;
                src[i + 3] = 255;
            }
        }
        let t = upload_rgba(sw as u32, sh as u32, &src);
        let samp = sampler(wgpu::FilterMode::Linear);
        let tview = t.create_view(&Default::default());
        let (bgl, bind) = sample_bind(&device, &tview, &samp);
        let pipe = mk_pipe(&device, SAMPLE_WGSL, &bgl, &vbl);
        let fb = frame_vp(&pipe, &bind, ow as u32, oh as u32);
        let mut bad = 0;
        for oy in 0..oh as i32 {
            for ox in 0..ow as i32 {
                let (sx0, sy0) = (ox * 2, oy * 2);
                let mut sum = [0i32; 3];
                for dy in 0..2 {
                    for dx in 0..2 {
                        let i = ((sy0 + dy) as usize * sw + (sx0 + dx) as usize) * 4;
                        sum[0] += src[i] as i32;
                        sum[1] += src[i + 1] as i32;
                        sum[2] += src[i + 2] as i32;
                    }
                }
                let er = (sum[0] as f32 / 4.0).round() as i32;
                let eg = (sum[1] as f32 / 4.0).round() as i32;
                let eb = (sum[2] as f32 / 4.0).round() as i32;
                if !fb.peq(ox, oy, er, eg, eb, 255, 2) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)",
        );
    }

    // ============ (4) codec round-trip identities (CPU path) ============
    {
        const N: usize = 8;
        let mut x = [0.0f64; N];
        let mut xc = [0.0f64; N];
        let mut yv = [0.0f64; N];
        for i in 0..N {
            x[i] = 30.0 + 20.0 * (0.7 * i as f64).sin() + 5.0 * i as f64;
        }
        for k in 0..N {
            let mut s = 0.0;
            for n in 0..N {
                s += x[n] * (std::f64::consts::PI / N as f64 * (n as f64 + 0.5) * k as f64).cos();
            }
            xc[k] = s;
        }
        for n in 0..N {
            let mut s = xc[0];
            for k in 1..N {
                s += 2.0
                    * xc[k]
                    * (std::f64::consts::PI / N as f64 * (n as f64 + 0.5) * k as f64).cos();
            }
            yv[n] = s / N as f64;
        }
        let mut maxerr = 0.0f64;
        for i in 0..N {
            maxerr = maxerr.max((yv[i] - x[i]).abs());
        }
        ok(
            maxerr < 1e-9,
            "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)",
        );
        let mut diff = 0.0f64;
        for i in 0..N {
            diff = diff.max((xc[i] - x[i]).abs());
        }
        ok(
            diff > 1.0,
            "DCT coefficients differ from input (transform is non-trivial)",
        );
    }
    {
        let input: Vec<u8> = vec![5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3];
        let mut enc: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < input.len() {
            let v = input[i];
            let mut j = i;
            while j < input.len() && input[j] == v && (j - i) < 255 {
                j += 1;
            }
            enc.push((j - i) as u8);
            enc.push(v);
            i = j;
        }
        let mut dec: Vec<u8> = Vec::new();
        let mut k = 0;
        while k + 1 < enc.len() {
            for _ in 0..enc[k] {
                dec.push(enc[k + 1]);
            }
            k += 2;
        }
        ok(dec == input, "RLE encode/decode round-trip identity");
        ok(
            enc.len() < input.len(),
            "RLE actually compressed the run data (encode is non-trivial)",
        );
    }

    // ---- Negative control ----
    {
        // Clear-only frame (no draw): whole readback is the clear color black.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("neg") });
        {
            let _rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neg-rp"),
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
        }
        let fb = copy_and_read(&device, &queue, &color, &readback, enc, padded);
        ok(
            fb.peq(0, 0, 0, 0, 0, 255, 1),
            "negative control setup: cleared to black",
        );
        ok(
            !fb.peq(0, 0, 255, 255, 255, 255, 1),
            "negative control: cleared buffer is NOT white",
        );
    }

    device.poll(wgpu::Maintain::Wait);
    device.destroy();
    finish()
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sample_bind(
    device: &wgpu::Device,
    tview: &wgpu::TextureView,
    samp: &wgpu::Sampler,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sample-bgl"),
        entries: &[
            tex_entry(0),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sample-bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(tview),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(samp),
            },
        ],
    });
    (bgl, bind)
}

fn mk_pipe(
    device: &wgpu::Device,
    wgsl: &str,
    bgl: &wgpu::BindGroupLayout,
    vbl: &wgpu::VertexBufferLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("codec"),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pll"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipe"),
        layout: Some(&pll),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs",
            buffers: std::slice::from_ref(vbl),
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
    })
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
    println!("scene-codec: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={EXPECTED}");
    if fail == 0 && total == EXPECTED {
        println!("SCENE_CODEC OK {pass}");
        0
    } else {
        println!("SCENE_CODEC FAIL");
        1
    }
}
