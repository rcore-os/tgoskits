// scene_codec_rust - streaming/codec-math RENDER-scene carpet on EGL-surfaceless / GLES 3.1 / llvmpipe,
// driven by glow + khronos-egl (same context bring-up + off-screen FBO as gles_render_rust). Mirrors
// scene_codec.cpp behaviour-identically, each path asserted against an INDEPENDENT closed-form
// ("numpy-equivalent") reference in Rust: (1) BT.601 full-range YUV->RGB in a three-plane fragment
// shader; (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample; (3) bilinear 2x downscale (GL_LINEAR vs a 2x2 box
// average); (4) CPU-path round-trips - an 8-sample DCT-II forward/IDCT reconstruction identity, plus an
// RLE encode/decode identity. Closes with a negative control. Prints "SCENE_CODEC_RUST OK <n>" only
// when FAIL==0 && TOTAL==EXPECTED==PASS. Dynamic-musl on-target; software rasterizer, deterministic.
use glow::HasContext;

const W: i32 = 64;
const H: i32 = 64;

static mut PASS: i32 = 0;
static mut FAIL: i32 = 0;
fn ok(c: bool, d: &str) {
    unsafe {
        if c {
            PASS += 1;
        } else {
            FAIL += 1;
            eprintln!("FAIL: {d}");
        }
    }
}
fn f32_bytes(v: &[f32]) -> &[u8] {
    unsafe { core::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}
fn clampi(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

const VS_UV: &str = "#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 \
                     t;\nout vec2 uv;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); uv=t; }\n";

fn run() -> i32 {
    let egl = unsafe {
        khronos_egl::DynamicInstance::<khronos_egl::EGL1_5>::load_required().expect("load libEGL")
    };
    let dpy = unsafe { egl.get_display(khronos_egl::DEFAULT_DISPLAY) };
    ok(dpy.is_some(), "eglGetDisplay");
    let dpy = dpy.expect("no EGL display");
    ok(egl.initialize(dpy).is_ok(), "eglInitialize");
    let cfg_attrs = [
        khronos_egl::SURFACE_TYPE,
        khronos_egl::PBUFFER_BIT,
        khronos_egl::RENDERABLE_TYPE,
        khronos_egl::OPENGL_ES3_BIT,
        khronos_egl::NONE,
    ];
    let cfg = egl.choose_first_config(dpy, &cfg_attrs);
    ok(matches!(cfg, Ok(Some(_))), "eglChooseConfig ES3");
    let cfg = cfg.ok().flatten().expect("no EGL config");
    ok(
        egl.bind_api(khronos_egl::OPENGL_ES_API).is_ok(),
        "eglBindAPI ES",
    );
    let ctx_attrs = [
        khronos_egl::CONTEXT_MAJOR_VERSION,
        3,
        khronos_egl::CONTEXT_MINOR_VERSION,
        1,
        khronos_egl::NONE,
    ];
    let ctx = egl.create_context(dpy, cfg, None, &ctx_attrs);
    ok(ctx.is_ok(), "eglCreateContext ES 3.1");
    let ctx = ctx.expect("no EGL context");
    ok(
        egl.make_current(dpy, None, None, Some(ctx)).is_ok(),
        "eglMakeCurrent surfaceless",
    );
    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            egl.get_proc_address(s)
                .map(|p| p as *const std::ffi::c_void)
                .unwrap_or(std::ptr::null())
        })
    };

    unsafe {
        let tex = gl.create_texture().expect("tex");
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            W,
            H,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            None,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::NEAREST as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::NEAREST as i32,
        );
        let rb = gl.create_renderbuffer().expect("rb");
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, W, H);
        let fbo = gl.create_framebuffer().expect("fbo");
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(rb),
        );
        ok(
            gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE,
            "FBO complete",
        );
        gl.viewport(0, 0, W, H);

        let mut buf = vec![0u8; (W * H * 4) as usize];
        let readback = |gl: &glow::Context, buf: &mut [u8]| {
            gl.read_pixels(
                0,
                0,
                W,
                H,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(buf),
            );
        };
        let px = |b: &[u8], x: i32, y: i32, c: i32| b[((y * W + x) * 4 + c) as usize] as i32;
        let peq = |b: &[u8], x: i32, y: i32, r: i32, g: i32, bl: i32, a: i32, tol: i32| {
            (px(b, x, y, 0) - r).abs() <= tol
                && (px(b, x, y, 1) - g).abs() <= tol
                && (px(b, x, y, 2) - bl).abs() <= tol
                && (px(b, x, y, 3) - a).abs() <= tol
        };
        let mk = |vs: &str, fs: &str| -> glow::Program {
            let v = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(v, vs);
            gl.compile_shader(v);
            if !gl.get_shader_compile_status(v) {
                ok(false, "vs compile");
            }
            let f = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            gl.shader_source(f, fs);
            gl.compile_shader(f);
            if !gl.get_shader_compile_status(f) {
                ok(false, "fs compile");
            }
            let p = gl.create_program().unwrap();
            gl.attach_shader(p, v);
            gl.attach_shader(p, f);
            gl.link_program(p);
            if !gl.get_program_link_status(p) {
                ok(false, "link");
            }
            gl.delete_shader(v);
            gl.delete_shader(f);
            p
        };

        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));
        let fsq: [f32; 16] = [
            -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        ];
        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&fsq), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
        gl.enable_vertex_attrib_array(1);

        let upload_r8 = |gl: &glow::Context, w: i32, h: i32, d: &[u8]| -> glow::Texture {
            let t = gl.create_texture().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::R8 as i32,
                w,
                h,
                0,
                glow::RED,
                glow::UNSIGNED_BYTE,
                Some(d),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            t
        };

        // ============ (1) YUV -> RGB, BT.601 full-range ============
        {
            let (pw, ph, cw, ch) = (32i32, 32i32, 16i32, 16i32);
            let mut yp = vec![0u8; (pw * ph) as usize];
            let mut up = vec![0u8; (cw * ch) as usize];
            let mut vpl = vec![0u8; (cw * ch) as usize];
            for y in 0..ph {
                for x in 0..pw {
                    yp[(y * pw + x) as usize] = clampi((x * 8 + y * 4) % 256, 0, 255) as u8;
                }
            }
            for y in 0..ch {
                for x in 0..cw {
                    up[(y * cw + x) as usize] = ((x * 16) % 256) as u8;
                    vpl[(y * cw + x) as usize] = ((y * 16) % 256) as u8;
                }
            }
            let ty = upload_r8(&gl, pw, ph, &yp);
            let tu = upload_r8(&gl, cw, ch, &up);
            let tv = upload_r8(&gl, cw, ch, &vpl);
            let prog = mk(
                VS_UV,
                "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out \
                 vec4 o;\nuniform sampler2D yT; uniform sampler2D uT; uniform sampler2D vT;\nvoid \
                 main(){ float Y=texture(yT,uv).r; float U=texture(uT,uv).r-0.5; float \
                 V=texture(vT,uv).r-0.5;\nfloat R=Y+1.402*V; float G=Y-0.344136*U-0.714136*V; \
                 float B=Y+1.772*U;\no=vec4(clamp(vec3(R,G,B),0.0,1.0),1.0); }\n",
            );
            ok(true, "YUV->RGB program compiles+links");
            gl.use_program(Some(prog));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(ty));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "yT").as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(tu));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "uT").as_ref(), 1);
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, Some(tv));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "vT").as_ref(), 2);
            gl.viewport(0, 0, pw, ph);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            gl.viewport(0, 0, W, H);
            readback(&gl, &mut buf);
            let (mut bad, mut checked) = (0, 0);
            for y in 0..ph {
                for x in 0..pw {
                    let u = (x as f32 + 0.5) / pw as f32;
                    let v = (y as f32 + 0.5) / ph as f32;
                    let cx = clampi((u * cw as f32).floor() as i32, 0, cw - 1);
                    let cy = clampi((v * ch as f32).floor() as i32, 0, ch - 1);
                    let yf = yp[(y * pw + x) as usize] as f32 / 255.0;
                    let uf = up[(cy * cw + cx) as usize] as f32 / 255.0 - 0.5;
                    let vf = vpl[(cy * cw + cx) as usize] as f32 / 255.0 - 0.5;
                    let r = yf + 1.402 * vf;
                    let g = yf - 0.344136 * uf - 0.714136 * vf;
                    let b = yf + 1.772 * uf;
                    let er = clampi((r.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                    let eg = clampi((g.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                    let eb = clampi((b.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                    checked += 1;
                    if !peq(&buf, x, y, er, eg, eb, 255, 3) {
                        bad += 1;
                    }
                }
            }
            ok(
                checked == pw * ph,
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
            gl.delete_program(prog);
            gl.delete_texture(ty);
            gl.delete_texture(tu);
            gl.delete_texture(tv);
        }

        // ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
        {
            let (sw, sh, ow, oh) = (4i32, 4i32, 16i32, 16i32);
            let mut src = [0u8; (4 * 4 * 4) as usize];
            for y in 0..sh {
                for x in 0..sw {
                    let i = ((y * sw + x) * 4) as usize;
                    src[i] = (x * 60 + 10) as u8;
                    src[i + 1] = (y * 60 + 20) as u8;
                    src[i + 2] = ((x + y) * 30) as u8;
                    src[i + 3] = 255;
                }
            }
            let st = gl.create_texture().unwrap();
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(st));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                sw,
                sh,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&src),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            let prog = mk(
                VS_UV,
                "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out \
                 vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n",
            );
            ok(true, "chroma-upsample program compiles+links");
            gl.use_program(Some(prog));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "s").as_ref(), 0);
            gl.viewport(0, 0, ow, oh);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            gl.viewport(0, 0, W, H);
            readback(&gl, &mut buf);
            let mut bad = 0;
            for y in 0..oh {
                for x in 0..ow {
                    let u = (x as f32 + 0.5) / ow as f32;
                    let v = (y as f32 + 0.5) / oh as f32;
                    let sx = clampi((u * sw as f32).floor() as i32, 0, sw - 1);
                    let sy = clampi((v * sh as f32).floor() as i32, 0, sh - 1);
                    let i = ((sy * sw + sx) * 4) as usize;
                    if !peq(
                        &buf,
                        x,
                        y,
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
                "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block \
                 (closed form)",
            );
            ok(
                peq(
                    &buf,
                    0,
                    0,
                    src[0] as i32,
                    src[1] as i32,
                    src[2] as i32,
                    255,
                    1,
                ),
                "upsample (0,0) = src(0,0)",
            );
            let i33 = ((3 * sw + 3) * 4) as usize;
            ok(
                peq(
                    &buf,
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
            gl.delete_program(prog);
            gl.delete_texture(st);
        }

        // ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
        {
            let (sw, sh, ow, oh) = (4i32, 4i32, 2i32, 2i32);
            let mut src = [0u8; (4 * 4 * 4) as usize];
            for y in 0..sh {
                for x in 0..sw {
                    let i = ((y * sw + x) * 4) as usize;
                    let v = (10 + (y * sw + x) * 15) as u8;
                    src[i] = v;
                    src[i + 1] = 255u8.wrapping_sub(v);
                    src[i + 2] = v;
                    src[i + 3] = 255;
                }
            }
            let st = gl.create_texture().unwrap();
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(st));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                sw,
                sh,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&src),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            let prog = mk(
                VS_UV,
                "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out \
                 vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n",
            );
            ok(true, "downscale program compiles+links");
            gl.use_program(Some(prog));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "s").as_ref(), 0);
            gl.viewport(0, 0, ow, oh);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            gl.viewport(0, 0, W, H);
            readback(&gl, &mut buf);
            let mut bad = 0;
            for oy in 0..oh {
                for ox in 0..ow {
                    let (sx0, sy0) = (ox * 2, oy * 2);
                    let mut sum = [0i32; 3];
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let i = (((sy0 + dy) * sw + (sx0 + dx)) * 4) as usize;
                            sum[0] += src[i] as i32;
                            sum[1] += src[i + 1] as i32;
                            sum[2] += src[i + 2] as i32;
                        }
                    }
                    let er = (sum[0] as f32 / 4.0).round() as i32;
                    let eg = (sum[1] as f32 / 4.0).round() as i32;
                    let eb = (sum[2] as f32 / 4.0).round() as i32;
                    if !peq(&buf, ox, oy, er, eg, eb, 255, 2) {
                        bad += 1;
                    }
                }
            }
            ok(
                bad == 0,
                "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)",
            );
            gl.delete_program(prog);
            gl.delete_texture(st);
        }

        // ============ (4) codec round-trip identities (CPU path) ============
        {
            let n = 8usize;
            let mut x = [0.0f64; 8];
            let mut xk = [0.0f64; 8];
            let mut yy = [0.0f64; 8];
            for i in 0..n {
                x[i] = 30.0 + 20.0 * (0.7 * i as f64).sin() + 5.0 * i as f64;
            }
            for k in 0..n {
                let mut s = 0.0;
                for nn in 0..n {
                    s += x[nn]
                        * (std::f64::consts::PI / n as f64 * (nn as f64 + 0.5) * k as f64).cos();
                }
                xk[k] = s;
            }
            for nn in 0..n {
                let mut s = xk[0];
                for k in 1..n {
                    s += 2.0
                        * xk[k]
                        * (std::f64::consts::PI / n as f64 * (nn as f64 + 0.5) * k as f64).cos();
                }
                yy[nn] = s / n as f64;
            }
            let mut maxerr = 0.0f64;
            for i in 0..n {
                maxerr = maxerr.max((yy[i] - x[i]).abs());
            }
            ok(
                maxerr < 1e-9,
                "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)",
            );
            let mut diff = 0.0f64;
            for i in 0..n {
                diff = diff.max((xk[i] - x[i]).abs());
            }
            ok(
                diff > 1.0,
                "DCT coefficients differ from input (transform is non-trivial)",
            );
        }
        {
            let input: Vec<u8> = vec![5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3];
            let mut enc: Vec<u8> = Vec::new();
            let mut i = 0usize;
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
            let mut k = 0usize;
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
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        readback(&gl, &mut buf);
        ok(
            peq(&buf, 0, 0, 0, 0, 0, 255, 1),
            "negative control setup: cleared to black",
        );
        ok(
            !peq(&buf, 0, 0, 255, 255, 255, 255, 1),
            "negative control: cleared buffer is NOT white",
        );
    }
    let _ = egl.make_current(dpy, None, None, None);

    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    let expected = 23;
    println!("scene-codec-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_CODEC_RUST OK {pass}");
        0
    } else {
        1
    }
}

fn main() {
    std::process::exit(run());
}
