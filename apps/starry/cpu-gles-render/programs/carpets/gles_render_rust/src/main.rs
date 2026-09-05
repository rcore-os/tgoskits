// gles_render_rust_full_api - OpenGL ES 3.1 RENDER carpet over surfaceless EGL, driven by glow +
// khronos-egl (#version 310 es shaders, ES3 context via eglBindAPI(OPENGL_ES_API)). Renders into an
// off-screen FBO (RGBA8 color texture + depth renderbuffer) and verifies the rasterizer per pixel with
// glReadPixels against a closed-form reference. Base primitives: clear-color, a solid quad through a
// compiled+linked program, an axis-aligned linear gradient (triangle-strip interpolates per-triangle,
// so only an axis-aligned gradient matches a full-quad closed form), a procedural checkerboard from
// gl_FragCoord, viewport restriction, scissor clears, the depth test (LESS occlusion), alpha blending
// (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all channels incl alpha), a 1x1 FBO, a sub-rectangle readback.
// Exhaustive per-API coverage: primitive topologies (indexed GL_TRIANGLES / GL_TRIANGLE_FAN / GL_LINES
// / GL_POINTS), a blend factor+equation matrix (ONE/ZERO, ONE/ONE, ZERO/ONE, DST_COLOR, GLES3-core MAX
// and FUNC_REVERSE_SUBTRACT), the full depth-func matrix (8 comparisons at window depth 0.75), face
// culling + winding (FRONT_AND_BACK / BACK with CCW vs CW), 2x2 texture upload+NEAREST sampling, and
// state queries (glIsEnabled / glGet GL_DEPTH_FUNC / glGetError), closing with a negative control.
// Prints "GLES_RENDER_RUST_FULL_API OK <n>" only when every assertion passes and count == EXPECTED.
// Dynamic-musl on-target (dlopens libEGL); software rasterizer (llvmpipe), no GPU.
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

const VS_POS: &str =
    "#version 310 es\nlayout(location=0) in vec2 p;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); }\n";
const VS_POS3: &str =
    "#version 310 es\nlayout(location=0) in vec3 p;\nvoid main(){ gl_Position=vec4(p,1.0); }\n";
const VS_COL: &str = "#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec4 \
                      c;\nout vec4 vc;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); vc=c; }\n";
const FS_UNI: &str = "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 \
                      o;\nuniform vec4 u;\nvoid main(){ o=u; }\n";
const FS_VCOL: &str = "#version 310 es\nprecision highp float;\nin vec4 vc;\nlayout(location=0) \
                       out vec4 o;\nvoid main(){ o=vc; }\n";
const FS_CHECK: &str = "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 \
                        o;\nvoid main(){ ivec2 c=ivec2(gl_FragCoord.xy); bool \
                        e=(((c.x>>3)+(c.y>>3))&1)==0; o=e?vec4(1.0):vec4(0.0,0.0,0.0,1.0); }\n";

fn run() -> i32 {
    // --- EGL surfaceless OpenGL ES 3.1 context (glow drives GLES the same way it drives desktop GL) ---
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
        ok(
            !gl.get_parameter_string(glow::VERSION).is_empty(),
            "GL VERSION string",
        );
        ok(
            !gl.get_parameter_string(glow::RENDERER).is_empty(),
            "GL RENDERER string",
        );

        // --- off-screen FBO: RGBA8 color texture + depth renderbuffer ---
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
        let all_eq = |b: &[u8], r: i32, g: i32, bl: i32, a: i32, tol: i32| {
            (0..H).all(|y| (0..W).all(|x| peq(b, x, y, r, g, bl, a, tol)))
        };

        // NDC fullscreen quad (triangle strip BL,BR,TL,TR)
        let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));
        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&quad), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        gl.enable_vertex_attrib_array(0);

        // --- Test 1: clear color ---
        gl.clear_color(0.0, 0.25, 0.5, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 0, 64, 128, 255, 2),
            "clear (0,0.25,0.5,1) all pixels (0,64,128,255)",
        );
        ok(peq(&buf, 0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)");
        ok(
            peq(&buf, W - 1, H - 1, 0, 64, 128, 255, 2),
            "clear pixel (63,63)",
        );

        // --- Test 2: solid quad ---
        let pu = mk(VS_POS, FS_UNI);
        ok(true, "solid program compiles+links");
        gl.use_program(Some(pu));
        let ul = gl.get_uniform_location(pu, "u");
        ok(ul.is_some(), "uniform u location");
        gl.uniform_4_f32(ul.as_ref(), 1.0, 0.0, 0.0, 1.0);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 255, 0, 0, 255, 1),
            "solid red quad fills every pixel",
        );
        ok(
            gl.get_error() == glow::NO_ERROR,
            "no GL error after solid draw",
        );

        // --- Test 3: axis-aligned linear gradient (horizontal red->blue) ---
        let gcol: [f32; 16] = [
            1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0,
        ];
        let cbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(cbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&gcol), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(1);
        let pg = mk(VS_COL, FS_VCOL);
        ok(true, "gradient program compiles+links");
        gl.use_program(Some(pg));
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        let mut bad = 0;
        for x in 0..W {
            let u = (x as f32 + 0.5) / W as f32;
            let r = (((1.0 - u) * 255.0).round()) as i32;
            let bl = ((u * 255.0).round()) as i32;
            for y in 0..H {
                if !peq(&buf, x, y, r, 0, bl, 255, 4) {
                    bad += 1;
                }
            }
        }
        ok(
            bad == 0,
            "gradient matches horizontal-linear closed-form for all pixels",
        );
        ok(
            peq(&buf, 0, 0, 255, 0, 0, 255, 8),
            "gradient left edge ~ red",
        );
        ok(
            peq(&buf, W - 1, H - 1, 0, 0, 255, 255, 8),
            "gradient right edge ~ blue",
        );
        ok(
            peq(&buf, W / 2, H / 2, 128, 0, 128, 255, 4),
            "gradient center ~ (128,0,128)",
        );
        gl.disable_vertex_attrib_array(1);

        // --- Test 4: checkerboard from gl_FragCoord ---
        let pc = mk(VS_POS, FS_CHECK);
        ok(true, "checker program compiles+links");
        gl.use_program(Some(pc));
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        let mut cbad = 0;
        for y in 0..H {
            for x in 0..W {
                let e = (((x >> 3) + (y >> 3)) & 1) == 0;
                let w = if e { 255 } else { 0 };
                if !peq(&buf, x, y, w, w, w, 255, 1) {
                    cbad += 1;
                }
            }
        }
        ok(
            cbad == 0,
            "checkerboard matches (x/8+y/8) parity for all pixels",
        );
        ok(
            peq(&buf, 0, 0, 255, 255, 255, 255, 1),
            "checker cell (0,0) white",
        );
        ok(peq(&buf, 8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black");

        // --- Test 5: viewport restriction ---
        gl.use_program(Some(pu));
        gl.uniform_4_f32(ul.as_ref(), 0.0, 1.0, 0.0, 1.0);
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.viewport(0, 0, W / 2, H / 2);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        gl.viewport(0, 0, W, H);
        readback(&gl, &mut buf);
        ok(
            peq(&buf, 5, 5, 0, 255, 0, 255, 1),
            "viewport: inside (5,5) green",
        );
        ok(
            peq(&buf, W - 5, H - 5, 255, 0, 0, 255, 1),
            "viewport: outside (59,59) red",
        );
        ok(
            peq(&buf, W / 2 + 2, H / 2 + 2, 255, 0, 0, 255, 1),
            "viewport: just outside quadrant red",
        );

        // --- Test 6: scissor-box clear ---
        gl.clear_color(0.0, 0.0, 1.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(16, 16, 32, 32);
        gl.clear_color(0.0, 1.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.disable(glow::SCISSOR_TEST);
        readback(&gl, &mut buf);
        ok(
            peq(&buf, 32, 32, 0, 255, 0, 255, 1),
            "scissor: inside box green",
        );
        ok(
            peq(&buf, 2, 2, 0, 0, 255, 255, 1),
            "scissor: outside box blue",
        );
        ok(
            peq(&buf, 50, 50, 0, 0, 255, 255, 1),
            "scissor: past box blue",
        );

        // --- Test 7: depth test (LESS occlusion) ---
        gl.enable(glow::DEPTH_TEST);
        gl.depth_func(glow::LESS);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear_depth_f32(1.0);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        let pd = mk(VS_POS3, FS_UNI);
        ok(true, "depth program compiles+links");
        gl.use_program(Some(pd));
        let ud = gl.get_uniform_location(pd, "u");
        let farq: [f32; 12] = [
            -1.0, -1.0, 0.5, 1.0, -1.0, 0.5, -1.0, 1.0, 0.5, 1.0, 1.0, 0.5,
        ];
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&farq), glow::DYNAMIC_DRAW);
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&farq), glow::DYNAMIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);
        gl.uniform_4_f32(ud.as_ref(), 1.0, 0.0, 0.0, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        let nearq: [f32; 12] = [
            0.0, -1.0, -0.5, 1.0, -1.0, -0.5, 0.0, 1.0, -0.5, 1.0, 1.0, -0.5,
        ];
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&nearq), glow::DYNAMIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);
        gl.uniform_4_f32(ud.as_ref(), 0.0, 1.0, 0.0, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            peq(&buf, W - 4, H / 2, 0, 255, 0, 255, 1),
            "depth: near green wins on right half",
        );
        ok(
            peq(&buf, 4, H / 2, 255, 0, 0, 255, 1),
            "depth: far red on left half",
        );
        gl.disable(glow::DEPTH_TEST);
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&quad), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);

        // --- Test 8: alpha blending ---
        gl.use_program(Some(pu));
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        gl.uniform_4_f32(ul.as_ref(), 0.0, 0.0, 1.0, 0.5);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        gl.disable(glow::BLEND);
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 128, 0, 128, 191, 3),
            "alpha blend 0.5*blue over red -> rgb(128,0,128) a191",
        );

        // --- Test 9: sub-rectangle readback ---
        gl.uniform_4_f32(ul.as_ref(), 0.2, 0.4, 0.6, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        let mut sub = vec![0u8; 4 * 4 * 4];
        gl.read_pixels(
            10,
            10,
            4,
            4,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(&mut sub),
        );
        let sub_ok = (0..16).all(|i| {
            (sub[i * 4] as i32 - 51).abs() <= 2
                && (sub[i * 4 + 1] as i32 - 102).abs() <= 2
                && (sub[i * 4 + 2] as i32 - 153).abs() <= 2
                && (sub[i * 4 + 3] as i32 - 255).abs() <= 2
        });
        ok(sub_ok, "sub-rect (10,10,4x4) == (51,102,153,255)");

        // --- Test 10: 1x1 FBO ---
        let t1 = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(t1));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            1,
            1,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            None,
        );
        let f1 = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f1));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(t1),
            0,
        );
        ok(
            gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE,
            "1x1 FBO complete",
        );
        gl.viewport(0, 0, 1, 1);
        gl.clear_color(0.5, 0.5, 0.5, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let mut one = [0u8; 4];
        gl.read_pixels(
            0,
            0,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(&mut one),
        );
        ok(
            (one[0] as i32 - 128).abs() <= 2
                && (one[1] as i32 - 128).abs() <= 2
                && (one[2] as i32 - 128).abs() <= 2,
            "1x1 pixel (128,128,128)",
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.viewport(0, 0, W, H);

        // ==================== exhaustive per-API render coverage ====================
        gl.use_program(Some(pu));
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        gl.enable_vertex_attrib_array(0);
        gl.uniform_4_f32(ul.as_ref(), 1.0, 0.0, 0.0, 1.0);

        // --- Test 11: primitive topologies (indexed triangles, fan, lines, points) ---
        {
            let ibo = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ibo));
            let idx: [u16; 6] = [0, 1, 2, 2, 1, 3];
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                core::slice::from_raw_parts(idx.as_ptr() as *const u8, 12),
                glow::STATIC_DRAW,
            );
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_elements(glow::TRIANGLES, 6, glow::UNSIGNED_SHORT, 0);
            gl.finish();
            readback(&gl, &mut buf);
            ok(
                all_eq(&buf, 255, 0, 0, 255, 1),
                "indexed GL_TRIANGLES fills quad",
            );
            gl.delete_buffer(ibo);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
        }
        {
            let fan: [f32; 12] = [
                0.0, 0.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0,
            ];
            let fb = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(fb));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&fan), glow::STATIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_FAN, 0, 6);
            gl.finish();
            readback(&gl, &mut buf);
            ok(
                all_eq(&buf, 255, 0, 0, 255, 1),
                "GL_TRIANGLE_FAN fills quad",
            );
            gl.delete_buffer(fb);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        }
        {
            let ln: [f32; 4] = [-1.0, 0.0, 1.0, 0.0];
            let lb = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(lb));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&ln), glow::STATIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::LINES, 0, 2);
            gl.finish();
            readback(&gl, &mut buf);
            let mid = (0..W)
                .filter(|&x| {
                    peq(&buf, x, H / 2, 255, 0, 0, 255, 2)
                        || peq(&buf, x, H / 2 - 1, 255, 0, 0, 255, 2)
                })
                .count();
            ok(mid as i32 >= W - 2, "GL_LINES draws the middle row");
            ok(
                peq(&buf, 0, H - 1, 0, 0, 0, 255, 2),
                "GL_LINES leaves top row clear",
            );
            gl.delete_buffer(lb);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        }
        {
            let pt: [f32; 2] = [0.0, 0.0];
            let pb = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(pb));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&pt), glow::STATIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::POINTS, 0, 1);
            gl.finish();
            readback(&gl, &mut buf);
            let mut hit = false;
            for y in H / 2 - 2..=H / 2 + 2 {
                for x in W / 2 - 2..=W / 2 + 2 {
                    if peq(&buf, x, y, 255, 0, 0, 255, 2) {
                        hit = true;
                    }
                }
            }
            ok(hit, "GL_POINTS draws a pixel at the center");
            gl.delete_buffer(pb);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        }

        // --- Test 12: blend factor + equation matrix (closed-form; MAX/REVERSE_SUBTRACT are GLES3 core) ---
        gl.enable(glow::BLEND);
        gl.blend_equation(glow::FUNC_ADD);
        gl.blend_func(glow::ONE, glow::ZERO);
        gl.clear_color(0.5, 0.5, 0.5, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.uniform_4_f32(ul.as_ref(), 0.0, 0.0, 1.0, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 0, 0, 255, 255, 2),
            "blend ONE/ZERO: src replaces dst",
        );
        gl.blend_func(glow::ONE, glow::ONE);
        gl.clear_color(0.5, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.uniform_4_f32(ul.as_ref(), 0.0, 0.0, 0.5, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 128, 0, 128, 255, 2),
            "blend ONE/ONE FUNC_ADD: src+dst = (128,0,128)",
        );
        gl.blend_func(glow::ZERO, glow::ONE);
        gl.clear_color(0.2, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.uniform_4_f32(ul.as_ref(), 0.0, 1.0, 0.0, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 51, 0, 0, 255, 2),
            "blend ZERO/ONE: dst kept (51,0,0)",
        );
        gl.blend_func(glow::DST_COLOR, glow::ZERO);
        gl.clear_color(0.5, 0.5, 0.5, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.uniform_4_f32(ul.as_ref(), 0.0, 0.0, 1.0, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 0, 0, 128, 255, 2),
            "blend DST_COLOR/ZERO: src*dst modulate (0,0,128)",
        );
        gl.blend_equation(glow::MAX);
        gl.blend_func(glow::ONE, glow::ONE);
        gl.clear_color(0.2, 0.6, 0.2, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.uniform_4_f32(ul.as_ref(), 0.6, 0.2, 0.6, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 153, 153, 153, 255, 2),
            "blend equation MAX: per-channel max",
        );
        gl.blend_equation(glow::FUNC_REVERSE_SUBTRACT);
        gl.blend_func(glow::ONE, glow::ONE);
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.uniform_4_f32(ul.as_ref(), 0.25, 0.0, 0.0, 1.0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 191, 0, 0, 0, 3),
            "blend equation REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0",
        );
        gl.blend_equation(glow::FUNC_ADD);
        gl.disable(glow::BLEND);

        // --- Test 13: depth-func matrix (NDC z=0.5 -> window depth 0.75; clear depth 0.75) ---
        gl.enable(glow::DEPTH_TEST);
        gl.depth_mask(true);
        {
            let dq: [f32; 12] = [
                -1.0, -1.0, 0.5, 1.0, -1.0, 0.5, -1.0, 1.0, 0.5, 1.0, 1.0, 0.5,
            ];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&dq), glow::DYNAMIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);
            gl.use_program(Some(pd));
            let udd = gl.get_uniform_location(pd, "u");
            let dt: [(u32, bool, &str); 8] = [
                (glow::ALWAYS, true, "always"),
                (glow::NEVER, false, "never"),
                (glow::LESS, false, "less"),
                (glow::LEQUAL, true, "lequal"),
                (glow::EQUAL, true, "equal"),
                (glow::GREATER, false, "greater"),
                (glow::GEQUAL, true, "gequal"),
                (glow::NOTEQUAL, false, "notequal"),
            ];
            for (f, draws, name) in dt {
                gl.depth_func(f);
                gl.clear_color(0.0, 0.0, 0.0, 1.0);
                gl.clear_depth_f32(0.75);
                gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
                gl.uniform_4_f32(udd.as_ref(), 0.0, 1.0, 0.0, 1.0);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                gl.finish();
                readback(&gl, &mut buf);
                let drew = peq(&buf, W / 2, H / 2, 0, 255, 0, 255, 2);
                ok(drew == draws, name);
            }
        }
        gl.disable(glow::DEPTH_TEST);
        gl.depth_func(glow::LESS);
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&quad), glow::STATIC_DRAW);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);

        // --- Test 14: face culling + winding ---
        gl.use_program(Some(pu));
        gl.uniform_4_f32(ul.as_ref(), 1.0, 0.0, 0.0, 1.0);
        gl.disable(glow::CULL_FACE);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(all_eq(&buf, 255, 0, 0, 255, 1), "cull disabled: quad drawn");
        gl.enable(glow::CULL_FACE);
        gl.cull_face(glow::FRONT_AND_BACK);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            all_eq(&buf, 0, 0, 0, 255, 1),
            "cull FRONT_AND_BACK: nothing drawn",
        );
        {
            gl.cull_face(glow::BACK);
            gl.front_face(glow::CCW);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            readback(&gl, &mut buf);
            let ccw = peq(&buf, W / 2, H / 2, 255, 0, 0, 255, 2);
            gl.front_face(glow::CW);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            readback(&gl, &mut buf);
            let cw = peq(&buf, W / 2, H / 2, 255, 0, 0, 255, 2);
            ok(ccw != cw, "cull BACK: CCW vs CW winding flips visibility");
        }
        gl.disable(glow::CULL_FACE);
        gl.front_face(glow::CCW);

        // --- Test 15: texture upload + sampling (2x2 texels, NEAREST) ---
        {
            let vs_tex = "#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in \
                          vec2 t;\nout vec2 uv;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); uv=t; \
                          }\n";
            let fs_tex = "#version 310 es\nprecision highp float;\nin vec2 \
                          uv;\nlayout(location=0) out vec4 o;\nuniform sampler2D s;\nvoid main(){ \
                          o=texture(s,uv); }\n";
            let ptx = mk(vs_tex, fs_tex);
            ok(true, "texture program compiles+links");
            let tx: [u8; 16] = [
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ];
            let smp = gl.create_texture().unwrap();
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(smp));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                2,
                2,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&tx),
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
            let tq: [f32; 16] = [
                -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
            ];
            let tb = gl.create_buffer().unwrap();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(tb));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&tq), glow::STATIC_DRAW);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
            gl.enable_vertex_attrib_array(1);
            gl.use_program(Some(ptx));
            gl.uniform_1_i32(gl.get_uniform_location(ptx, "s").as_ref(), 0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.finish();
            readback(&gl, &mut buf);
            ok(
                peq(&buf, W / 4, H / 4, 255, 0, 0, 255, 2),
                "texture NEAREST bottom-left red",
            );
            ok(
                peq(&buf, 3 * W / 4, H / 4, 0, 255, 0, 255, 2),
                "texture NEAREST bottom-right green",
            );
            ok(
                peq(&buf, W / 4, 3 * H / 4, 0, 0, 255, 255, 2),
                "texture NEAREST top-left blue",
            );
            ok(
                peq(&buf, 3 * W / 4, 3 * H / 4, 255, 255, 255, 255, 2),
                "texture NEAREST top-right white",
            );
            gl.delete_texture(smp);
            gl.delete_buffer(tb);
            gl.disable_vertex_attrib_array(1);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.enable_vertex_attrib_array(0);
        }

        // --- Test 16: state queries (glGet*, glIsEnabled) reflect the render state ---
        gl.enable(glow::BLEND);
        ok(
            gl.is_enabled(glow::BLEND),
            "glIsEnabled(GL_BLEND) true after enable",
        );
        gl.disable(glow::BLEND);
        ok(
            !gl.is_enabled(glow::BLEND),
            "glIsEnabled(GL_BLEND) false after disable",
        );
        gl.depth_func(glow::LEQUAL);
        ok(
            gl.get_parameter_i32(glow::DEPTH_FUNC) == glow::LEQUAL as i32,
            "glGet GL_DEPTH_FUNC == LEQUAL",
        );
        gl.depth_func(glow::LESS);
        ok(
            gl.get_error() == glow::NO_ERROR,
            "glGetError == GL_NO_ERROR after full render suite",
        );

        // --- Negative control ---
        gl.use_program(Some(pu));
        gl.uniform_4_f32(ul.as_ref(), 1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            !all_eq(&buf, 0, 255, 0, 255, 2),
            "negative control: red buffer is NOT green",
        );
        ok(
            !peq(&buf, 0, 0, 0, 0, 0, 255, 2),
            "negative control: red pixel is NOT black",
        );
    }
    let _ = egl.make_current(dpy, None, None, None);

    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    let expected = 71;
    println!("gles-render-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("GLES_RENDER_RUST_FULL_API OK {pass}");
        0
    } else {
        1
    }
}

fn main() {
    std::process::exit(run());
}
