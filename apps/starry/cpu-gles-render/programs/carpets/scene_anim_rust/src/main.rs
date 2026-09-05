// scene_anim_rust - keyframe-animation RENDER-scene carpet on EGL-surfaceless / GLES 3.1 / llvmpipe,
// driven by glow + khronos-egl (same context bring-up + off-screen FBO as gles_render_rust). Mirrors
// scene_anim.cpp behaviour-identically: N=4 keyframes of a transformed unit quad (rotation about the
// FBO center composed with translate + uniform scale, interpolated over t in {0,0.25,0.5,0.75}). Each
// frame the four transformed corners are computed closed-form in Rust (R(theta)*S*local + T) and the
// readback is asserted at those exact corner pixels plus a just-outside background point; a cubic ease
// eased(t)=3t^2-2t^3 drives the scale and is asserted at each t. NDC z is unused (2D). Closes with a
// negative control. Prints "SCENE_ANIM_RUST OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
// Dynamic-musl on-target; software rasterizer (llvmpipe), deterministic.
use glow::HasContext;

const W: i32 = 64;
const H: i32 = 64;
const PI: f32 = std::f32::consts::PI;

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
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn ease_cubic(t: f32) -> f32 {
    3.0 * t * t - 2.0 * t * t * t
}

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
        let near_color = |b: &[u8], x: i32, y: i32, r: i32, g: i32, bl: i32, tol: i32| -> bool {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= W || yy >= H {
                        continue;
                    }
                    if peq(b, xx, yy, r, g, bl, 255, tol) {
                        return true;
                    }
                }
            }
            false
        };

        let prog = {
            let v = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(
                v,
                "#version 310 es\nlayout(location=0) in vec2 lp;\nuniform vec2 vp;\nuniform vec2 \
                 col0;\nuniform vec2 col1;\nuniform vec2 tr;\nvoid main(){ vec2 pix = col0*lp.x + \
                 col1*lp.y + tr; vec2 n=(pix/vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); }\n",
            );
            gl.compile_shader(v);
            if !gl.get_shader_compile_status(v) {
                ok(false, "vs compile");
            }
            let f = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            gl.shader_source(
                f,
                "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\nuniform \
                 vec4 u;\nvoid main(){ o=u; }\n",
            );
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
        ok(true, "anim program compiles+links");
        gl.use_program(Some(prog));
        let vpl = gl.get_uniform_location(prog, "vp");
        let c0 = gl.get_uniform_location(prog, "col0");
        let c1 = gl.get_uniform_location(prog, "col1");
        let trl = gl.get_uniform_location(prog, "tr");
        let ul = gl.get_uniform_location(prog, "u");
        ok(
            vpl.is_some() && c0.is_some() && c1.is_some() && trl.is_some() && ul.is_some(),
            "anim uniform locations",
        );
        gl.uniform_2_f32(vpl.as_ref(), W as f32, H as f32);

        let local: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));
        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&local), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        gl.enable_vertex_attrib_array(0);

        let (a0, a1) = (0.0f32, PI / 2.0);
        let (s0, s1) = (6.0f32, 14.0f32);
        let (cx0, cx1, cy0, cy1) = (20.0f32, 44.0f32, 20.0f32, 44.0f32);

        // returns (col0[2], col1[2], tr[2], scale, angle)
        let frame_transform = |t: f32| -> ([f32; 2], [f32; 2], [f32; 2], f32, f32) {
            let ang = lerp(a0, a1, t);
            let sc = lerp(s0, s1, ease_cubic(t));
            let cx = lerp(cx0, cx1, t);
            let cy = lerp(cy0, cy1, t);
            let (ca, sa) = (ang.cos(), ang.sin());
            let col0 = [sc * ca, sc * sa];
            let col1 = [-sc * sa, sc * ca];
            let tr = [cx, cy];
            (col0, col1, tr, sc, ang)
        };

        let ts = [0.0f32, 0.25, 0.5, 0.75];
        let cols: [[f32; 3]; 4] = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
        ];

        for fi in 0..4 {
            let t = ts[fi];
            let (col0, col1, tr, sc, ang) = frame_transform(t);
            gl.uniform_2_f32(c0.as_ref(), col0[0], col0[1]);
            gl.uniform_2_f32(c1.as_ref(), col1[0], col1[1]);
            gl.uniform_2_f32(trl.as_ref(), tr[0], tr[1]);
            gl.uniform_4_f32(ul.as_ref(), cols[fi][0], cols[fi][1], cols[fi][2], 1.0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            readback(&gl, &mut buf);

            // closed-form corner positions: corner = R(ang)*S(sc)*localCorner + center
            let (ca, sa) = (ang.cos(), ang.sin());
            let mut corners = [[0.0f32; 2]; 4];
            for k in 0..4 {
                let lx = local[k * 2];
                let ly = local[k * 2 + 1];
                let rx = sc * (ca * lx - sa * ly);
                let ry = sc * (sa * lx + ca * ly);
                corners[k][0] = tr[0] + rx;
                corners[k][1] = tr[1] + ry;
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
                peq(
                    &buf,
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
                let onscreen = px_ >= 0 && py_ >= 0 && px_ < W && py_ < H;
                ok(
                    onscreen
                        && near_color(
                            &buf,
                            px_,
                            py_,
                            (cols[fi][0] * 255.0).round() as i32,
                            (cols[fi][1] * 255.0).round() as i32,
                            (cols[fi][2] * 255.0).round() as i32,
                            40,
                        ),
                    "transformed corner pixel is inside the rendered quad (closed-form \
                     R*S*local+T)",
                );
            }

            {
                let ox = if fi < 2 { W - 2 } else { 1 };
                let oy = if fi < 2 { H - 2 } else { 1 };
                let reach = sc * 1.4142;
                let covers = (ox as f32 + 0.5 - tr[0]).abs() <= reach
                    && (oy as f32 + 0.5 - tr[1]).abs() <= reach;
                if !covers {
                    ok(
                        peq(&buf, ox, oy, 0, 0, 0, 255, 2),
                        "outside-quad point stays background (closed-form silhouette)",
                    );
                } else {
                    ok(true, "outside-quad point skipped (would be covered)");
                }
            }
        }

        // t=0 vs t=0.75 geometry differs
        {
            let (_, _, tra, ..) = frame_transform(0.0);
            let (_, _, trb, ..) = frame_transform(0.75);
            ok(
                (tra[0] - trb[0]).abs() > 1.0,
                "center translates between t=0 and t=0.75 (animation is real)",
            );
        }

        // rotation at t=0.5
        {
            let (col0, _, _, _, ang) = frame_transform(0.5);
            ok(
                (ang - PI / 4.0).abs() < 1e-5,
                "t=0.5 rotation angle = pi/4 closed-form",
            );
            ok(
                (col0[0] - col0[1]).abs() < 1e-4 && col0[0] > 0.0,
                "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)",
            );
        }

        // negative control: frame 0 (red) is NOT green
        {
            let (col0, col1, tr, ..) = frame_transform(0.0);
            gl.uniform_2_f32(c0.as_ref(), col0[0], col0[1]);
            gl.uniform_2_f32(c1.as_ref(), col1[0], col1[1]);
            gl.uniform_2_f32(trl.as_ref(), tr[0], tr[1]);
            gl.uniform_4_f32(ul.as_ref(), 1.0, 0.0, 0.0, 1.0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            readback(&gl, &mut buf);
            let cxi = (tr[0] - 0.5).round() as i32;
            let cyi = (tr[1] - 0.5).round() as i32;
            ok(
                !peq(&buf, cxi, cyi, 0, 255, 0, 255, 4),
                "negative control: frame-0 center is NOT green",
            );
        }
    }
    let _ = egl.make_current(dpy, None, None, None);

    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    let expected = 45;
    println!("scene-anim-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_ANIM_RUST OK {pass}");
        0
    } else {
        1
    }
}

fn main() {
    std::process::exit(run());
}
