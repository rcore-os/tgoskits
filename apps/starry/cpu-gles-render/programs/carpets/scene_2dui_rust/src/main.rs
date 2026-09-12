// scene_2dui_rust - 2D UI compositing RENDER-scene carpet on EGL-surfaceless / GLES 3.1 / llvmpipe,
// driven by glow + khronos-egl (the same context bring-up + off-screen FBO + glReadPixels harness as
// gles_render_rust). Mirrors scene_2dui.cpp behaviour-identically: an orthographic pixel-space
// projection, and every scene primitive verified against an INDEPENDENT closed-form software reference
// (never derived from the GL output) - filled rectangles, an analytic rounded-rect (inside/corner-arc/
// outside), a nine-patch scaled border frame, an 8x8 bitmap-font glyph blit (every lit/unlit texel), a
// scissor-clipped fill, and MULTI-LAYER Porter-Duff over compositing of 3 stacked semi-transparent
// layers Co = Cs*As + Cd*(1-As) matched channel-by-channel incl alpha. Closes with a negative control.
// Prints "SCENE_2DUI_RUST OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Dynamic-musl on-target
// (dlopens libEGL); software rasterizer (llvmpipe), single-threaded, deterministic.
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
fn q8(f: f32) -> i32 {
    clampi((f * 255.0).round() as i32, 0, 255)
}

// pixel-space quad -> NDC (bottom-origin, matches gl_FragCoord + glReadPixels; no flip).
const VS_PIX: &str = "#version 310 es\nlayout(location=0) in vec2 p;\nuniform vec2 vp;\nvoid \
                      main(){ vec2 n = (p/vp)*2.0 - 1.0; gl_Position=vec4(n,0.0,1.0); }\n";
const FS_UNI: &str = "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 \
                      o;\nuniform vec4 u;\nvoid main(){ o=u; }\n";

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
        // off-screen FBO: RGBA8 color texture + DEPTH24 renderbuffer
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

        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));
        let vbo = gl.create_buffer().unwrap();

        // pixel-space filled rectangle [x0,x1)x[y0,y1) via two triangles (glow variant of fill_rect).
        let fill_rect = |gl: &glow::Context,
                         u_loc: Option<&glow::UniformLocation>,
                         x0: f32,
                         y0: f32,
                         x1: f32,
                         y1: f32,
                         r: f32,
                         g: f32,
                         b: f32,
                         a: f32| {
            let v: [f32; 12] = [x0, y0, x1, y0, x0, y1, x0, y1, x1, y0, x1, y1];
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&v), glow::DYNAMIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.enable_vertex_attrib_array(0);
            gl.uniform_4_f32(u_loc, r, g, b, a);
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
        };

        let prog = mk(VS_PIX, FS_UNI);
        ok(true, "pixel-fill program compiles+links");
        gl.use_program(Some(prog));
        let vp_loc = gl.get_uniform_location(prog, "vp");
        let u_loc = gl.get_uniform_location(prog, "u");
        ok(vp_loc.is_some() && u_loc.is_some(), "uniform locations");
        gl.uniform_2_f32(vp_loc.as_ref(), W as f32, H as f32);
        ok(gl.get_error() == glow::NO_ERROR, "no GL error after setup");

        // ---- Scene A: filled rectangles ----
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        fill_rect(
            &gl,
            u_loc.as_ref(),
            8.0,
            8.0,
            16.0,
            24.0,
            1.0,
            0.0,
            0.0,
            1.0,
        );
        fill_rect(
            &gl,
            u_loc.as_ref(),
            40.0,
            32.0,
            48.0,
            52.0,
            0.0,
            1.0,
            0.0,
            1.0,
        );
        gl.finish();
        readback(&gl, &mut buf);
        {
            let mut bad = 0;
            for y in 0..H {
                for x in 0..W {
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
                    if !peq(&buf, x, y, er, eg, eb, 255, 1) {
                        bad += 1;
                    }
                }
            }
            ok(
                bad == 0,
                "filled rectangles: every pixel matches closed-form rect coverage",
            );
            ok(peq(&buf, 10, 10, 255, 0, 0, 255, 1), "rect A interior red");
            ok(
                peq(&buf, 44, 40, 0, 255, 0, 255, 1),
                "rect B interior green",
            );
            ok(
                peq(&buf, 30, 30, 0, 0, 0, 255, 1),
                "gap between rects is background",
            );
        }

        // ---- Scene B: analytic rounded-rect ----
        {
            let fs_rr =
                "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\nuniform \
                 vec4 box;\nuniform float rad;\nuniform vec4 col;\nvoid main(){ vec2 \
                 p=gl_FragCoord.xy; float x0=box.x,y0=box.y,x1=box.z,y1=box.w;\nbool inside = \
                 p.x>=x0&&p.x<x1&&p.y>=y0&&p.y<y1;\nif(!inside){ discard; }\nvec2 c = p; bool \
                 corner=false; vec2 \
                 cc=vec2(0.0);\nif(p.x<x0+rad&&p.y<y0+rad){corner=true;cc=vec2(x0+rad,y0+rad);}\\
                 nelse if(p.x>=x1-rad&&p.y<y0+rad){corner=true;cc=vec2(x1-rad,y0+rad);}\nelse \
                 if(p.x<x0+rad&&p.y>=y1-rad){corner=true;cc=vec2(x0+rad,y1-rad);}\nelse \
                 if(p.x>=x1-rad&&p.y>=y1-rad){corner=true;cc=vec2(x1-rad,y1-rad);}\nif(corner && \
                 distance(c,cc)>rad){ discard; }\no=col; }\n";
            let prr = mk(VS_PIX, fs_rr);
            ok(true, "rounded-rect program compiles+links");
            gl.use_program(Some(prr));
            gl.uniform_2_f32(
                gl.get_uniform_location(prr, "vp").as_ref(),
                W as f32,
                H as f32,
            );
            gl.uniform_4_f32(
                gl.get_uniform_location(prr, "box").as_ref(),
                12.0,
                12.0,
                52.0,
                52.0,
            );
            gl.uniform_1_f32(gl.get_uniform_location(prr, "rad").as_ref(), 8.0);
            gl.uniform_4_f32(
                gl.get_uniform_location(prr, "col").as_ref(),
                1.0,
                1.0,
                0.0,
                1.0,
            );
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let fq: [f32; 12] = [
                0.0, 0.0, W as f32, 0.0, 0.0, H as f32, 0.0, H as f32, W as f32, 0.0, W as f32,
                H as f32,
            ];
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&fq), glow::DYNAMIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
            gl.finish();
            readback(&gl, &mut buf);
            let covered = |x: i32, y: i32| -> bool {
                let cx = x as f32 + 0.5;
                let cy = y as f32 + 0.5;
                let (x0, y0, x1, y1, r) = (12.0f32, 12.0f32, 52.0f32, 52.0f32, 8.0f32);
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
                    let dx = cx - ccx;
                    let dy = cy - ccy;
                    if (dx * dx + dy * dy).sqrt() > r {
                        return false;
                    }
                }
                true
            };
            let mut bad = 0;
            let mut lit = 0;
            for y in 0..H {
                for x in 0..W {
                    let cov = covered(x, y);
                    if cov {
                        lit += 1;
                    }
                    let (er, eg, eb) = if cov { (255, 255, 0) } else { (0, 0, 0) };
                    if !peq(&buf, x, y, er, eg, eb, 255, 1) {
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
                peq(&buf, 32, 32, 255, 255, 0, 255, 1),
                "rounded-rect center lit",
            );
            ok(
                peq(&buf, 12, 12, 0, 0, 0, 255, 1),
                "rounded-rect clipped corner (12,12) is background",
            );
            ok(
                peq(&buf, 32, 13, 255, 255, 0, 255, 1),
                "rounded-rect straight top edge lit",
            );
            gl.delete_program(prr);
            gl.use_program(Some(prog));
        }

        // ---- Scene C: nine-patch-style scaled border frame ----
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        fill_rect(
            &gl,
            u_loc.as_ref(),
            4.0,
            4.0,
            60.0,
            60.0,
            0.0,
            0.0,
            1.0,
            1.0,
        );
        fill_rect(
            &gl,
            u_loc.as_ref(),
            10.0,
            10.0,
            54.0,
            54.0,
            0.1,
            0.1,
            0.1,
            1.0,
        );
        gl.finish();
        readback(&gl, &mut buf);
        {
            let mut bad = 0;
            for y in 0..H {
                for x in 0..W {
                    let inbox = x >= 4 && x < 60 && y >= 4 && y < 60;
                    let ininner = x >= 10 && x < 54 && y >= 10 && y < 54;
                    let (er, eg, eb);
                    if ininner {
                        er = q8(0.1);
                        eg = q8(0.1);
                        eb = q8(0.1);
                    } else if inbox {
                        er = 0;
                        eg = 0;
                        eb = 255;
                    } else {
                        er = 0;
                        eg = 0;
                        eb = 0;
                    }
                    if !peq(&buf, x, y, er, eg, eb, 255, 1) {
                        bad += 1;
                    }
                }
            }
            ok(
                bad == 0,
                "nine-patch border frame: closed-form border-vs-interior coverage",
            );
            ok(
                peq(&buf, 5, 32, 0, 0, 255, 255, 1),
                "nine-patch left border blue",
            );
            ok(
                peq(&buf, 32, 5, 0, 0, 255, 255, 1),
                "nine-patch top border blue",
            );
            ok(
                peq(&buf, 32, 32, q8(0.1), q8(0.1), q8(0.1), 255, 1),
                "nine-patch hollow interior",
            );
        }

        // ---- Scene D: 8x8 bitmap-font glyph blit ('H') ----
        let glyph_h: [u8; 8] = [0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00];
        {
            let mut rgba = [0u8; 8 * 8 * 4];
            for r in 0..8 {
                for c in 0..8 {
                    let lit = (glyph_h[r] >> (7 - c)) & 1 != 0;
                    let v = if lit { 255u8 } else { 0u8 };
                    let idx = (r * 8 + c) * 4;
                    rgba[idx] = v;
                    rgba[idx + 1] = v;
                    rgba[idx + 2] = v;
                    rgba[idx + 3] = 255;
                }
            }
            let gtex = gl.create_texture().unwrap();
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(gtex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                8,
                8,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&rgba),
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
            let ptex = mk(
                "#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 \
                 t;\nout vec2 uv;\nuniform vec2 vp;\nvoid main(){ vec2 n=(p/vp)*2.0-1.0; \
                 gl_Position=vec4(n,0.0,1.0); uv=t; }\n",
                "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out \
                 vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n",
            );
            ok(true, "glyph program compiles+links");
            gl.use_program(Some(ptex));
            gl.uniform_2_f32(
                gl.get_uniform_location(ptex, "vp").as_ref(),
                W as f32,
                H as f32,
            );
            gl.uniform_1_i32(gl.get_uniform_location(ptex, "s").as_ref(), 0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            let gq: [f32; 24] = [
                20.0, 20.0, 0.0, 0.0, 28.0, 20.0, 1.0, 0.0, 20.0, 28.0, 0.0, 1.0, 20.0, 28.0, 0.0,
                1.0, 28.0, 20.0, 1.0, 0.0, 28.0, 28.0, 1.0, 1.0,
            ];
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&gq), glow::DYNAMIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
            gl.enable_vertex_attrib_array(1);
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
            gl.finish();
            readback(&gl, &mut buf);
            let mut bad = 0;
            for dy in 0..8 {
                for dx in 0..8 {
                    let sx = 20 + dx;
                    let sy = 20 + dy;
                    let (trow, tcol) = (dy as usize, dx as usize);
                    let lit = (glyph_h[trow] >> (7 - tcol)) & 1 != 0;
                    let v = if lit { 255 } else { 0 };
                    if !peq(&buf, sx, sy, v, v, v, 255, 1) {
                        bad += 1;
                    }
                }
            }
            ok(
                bad == 0,
                "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap",
            );
            ok(
                peq(&buf, 21, 23, 255, 255, 255, 255, 1),
                "glyph crossbar lit (col1,row3)",
            );
            ok(peq(&buf, 23, 20, 0, 0, 0, 255, 1), "glyph row0 blank");
            ok(
                peq(&buf, 24, 21, 0, 0, 0, 255, 1),
                "glyph row1 middle blank (0x42)",
            );
            gl.delete_program(ptex);
            gl.delete_texture(gtex);
            gl.disable_vertex_attrib_array(1);
            gl.use_program(Some(prog));
        }

        // ---- Scene E: scissor-clipped fill ----
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(16, 16, 20, 20);
        fill_rect(
            &gl,
            u_loc.as_ref(),
            0.0,
            0.0,
            W as f32,
            H as f32,
            1.0,
            0.0,
            1.0,
            1.0,
        );
        gl.disable(glow::SCISSOR_TEST);
        gl.finish();
        readback(&gl, &mut buf);
        {
            let mut bad = 0;
            for y in 0..H {
                for x in 0..W {
                    let inr = x >= 16 && x < 36 && y >= 16 && y < 36;
                    let (er, eg, eb) = if inr { (255, 0, 255) } else { (0, 0, 0) };
                    if !peq(&buf, x, y, er, eg, eb, 255, 1) {
                        bad += 1;
                    }
                }
            }
            ok(
                bad == 0,
                "scissor-clipped fill: magenta only within [16,36)^2",
            );
            ok(
                peq(&buf, 20, 20, 255, 0, 255, 255, 1),
                "scissor inside magenta",
            );
            ok(
                peq(&buf, 40, 40, 0, 0, 0, 255, 1),
                "scissor outside background",
            );
        }

        // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
        {
            let bg = [0.10f32, 0.10, 0.10, 1.0];
            // layers: [r,g,b,a, x0,y0,x1,y1]
            let layers: [[f32; 8]; 3] = [
                [1.0, 0.0, 0.0, 0.50, 8.0, 8.0, 56.0, 56.0],
                [0.0, 1.0, 0.0, 0.25, 12.0, 12.0, 52.0, 52.0],
                [0.0, 0.0, 1.0, 0.75, 16.0, 16.0, 48.0, 48.0],
            ];
            gl.clear_color(bg[0], bg[1], bg[2], bg[3]);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.blend_equation(glow::FUNC_ADD);
            for l in &layers {
                fill_rect(
                    &gl,
                    u_loc.as_ref(),
                    l[4],
                    l[5],
                    l[6],
                    l[7],
                    l[0],
                    l[1],
                    l[2],
                    l[3],
                );
            }
            gl.disable(glow::BLEND);
            gl.finish();
            readback(&gl, &mut buf);
            let composite = |tx: i32, ty: i32| -> [f32; 4] {
                let mut c = bg;
                for l in &layers {
                    let cx = tx as f32 + 0.5;
                    let cy = ty as f32 + 0.5;
                    if cx >= l[4] && cx < l[6] && cy >= l[5] && cy < l[7] {
                        let as_ = l[3];
                        let src = [l[0], l[1], l[2], l[3]];
                        for k in 0..4 {
                            c[k] = src[k] * as_ + c[k] * (1.0 - as_);
                        }
                    }
                }
                c
            };
            let mut bad = 0;
            for y in 0..H {
                for x in 0..W {
                    let e = composite(x, y);
                    if !peq(&buf, x, y, q8(e[0]), q8(e[1]), q8(e[2]), q8(e[3]), 2) {
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
                    [1.0f32, 0.0, 0.0, 0.5],
                    [0.0, 1.0, 0.0, 0.25],
                    [0.0, 0.0, 1.0, 0.75],
                ];
                for li in &ls {
                    let as_ = li[3];
                    for k in 0..4 {
                        c[k] = li[k] * as_ + c[k] * (1.0 - as_);
                    }
                }
                ok(
                    peq(&buf, 32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2),
                    "multi-layer over center pixel matches hand-iterated over",
                );
            }
            {
                let as_ = 0.5f32;
                let er = 1.0 * as_ + bg[0] * (1.0 - as_);
                let eg = 0.0 * as_ + bg[1] * (1.0 - as_);
                let eb = 0.0 * as_ + bg[2] * (1.0 - as_);
                let ea = as_ * as_ + bg[3] * (1.0 - as_);
                ok(
                    peq(&buf, 10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2),
                    "multi-layer over: single-layer region matches one over",
                );
            }
        }

        // ---- Negative control ----
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        fill_rect(
            &gl,
            u_loc.as_ref(),
            8.0,
            8.0,
            16.0,
            24.0,
            1.0,
            0.0,
            0.0,
            1.0,
        );
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            !peq(&buf, 10, 10, 0, 255, 0, 255, 4),
            "negative control: red rect pixel is NOT green",
        );
        ok(
            !peq(&buf, 30, 30, 255, 0, 0, 255, 4),
            "negative control: background is NOT red",
        );
    }
    let _ = egl.make_current(dpy, None, None, None);

    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    let expected = 37;
    println!("scene-2dui-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_2DUI_RUST OK {pass}");
        0
    } else {
        1
    }
}

fn main() {
    std::process::exit(run());
}
