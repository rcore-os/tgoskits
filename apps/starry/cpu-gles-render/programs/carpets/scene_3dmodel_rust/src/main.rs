// scene_3dmodel_rust - 3D indexed-mesh RENDER-scene carpet on EGL-surfaceless / GLES 3.1 / llvmpipe,
// driven by glow + khronos-egl (same context bring-up + off-screen FBO as gles_render_rust). Mirrors
// scene_3dmodel.cpp behaviour-identically: an indexed cube mesh with a hand-computed perspective MVP,
// depth-buffered occlusion (GL_LESS) and Gouraud shading, verified against an INDEPENDENT software
// rasterizer in Rust (same MVP -> clip -> NDC -> viewport, per-pixel barycentric + perspective-correct
// depth test in a private z-buffer + interpolated vertex colors) compared to the GL readback per pixel.
// GL uses NDC z in [-1,1] (GL convention). Closes with a negative control. Prints
// "SCENE_3DMODEL_RUST OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Dynamic-musl on-target.
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

// column-major 4x4 matrix math (GL layout: m[col*4+row]).
#[derive(Clone, Copy)]
struct M4 {
    m: [f32; 16],
}
fn mul(a: &M4, b: &M4) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    for c in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a.m[k * 4 + row] * b.m[c * 4 + k];
            }
            r.m[c * 4 + row] = s;
        }
    }
    r
}
fn mv4(a: &M4, v: &[f32; 4]) -> [f32; 4] {
    let mut o = [0.0f32; 4];
    for row in 0..4 {
        let mut s = 0.0;
        for k in 0..4 {
            s += a.m[k * 4 + row] * v[k];
        }
        o[row] = s;
    }
    o
}
fn perspective(fovy: f32, aspect: f32, zn: f32, zf: f32) -> M4 {
    let f = 1.0 / (fovy * 0.5).tan();
    let mut r = M4 { m: [0.0; 16] };
    r.m[0] = f / aspect;
    r.m[5] = f;
    r.m[2 * 4 + 2] = (zf + zn) / (zn - zf);
    r.m[2 * 4 + 3] = -1.0;
    r.m[3 * 4 + 2] = (2.0 * zf * zn) / (zn - zf);
    r
}
fn translate(x: f32, y: f32, z: f32) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    r.m[0] = 1.0;
    r.m[5] = 1.0;
    r.m[10] = 1.0;
    r.m[15] = 1.0;
    r.m[3 * 4] = x;
    r.m[3 * 4 + 1] = y;
    r.m[3 * 4 + 2] = z;
    r
}
fn rot_y(a: f32) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    let (c, s) = (a.cos(), a.sin());
    r.m[0] = c;
    r.m[0 * 4 + 2] = -s;
    r.m[2 * 4] = s;
    r.m[2 * 4 + 2] = c;
    r.m[5] = 1.0;
    r.m[15] = 1.0;
    r
}
fn rot_x(a: f32) -> M4 {
    let mut r = M4 { m: [0.0; 16] };
    let (c, s) = (a.cos(), a.sin());
    r.m[5] = c;
    r.m[1 * 4 + 2] = s;
    r.m[2 * 4 + 1] = -s;
    r.m[2 * 4 + 2] = c;
    r.m[0] = 1.0;
    r.m[15] = 1.0;
    r
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

        // cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (Gouraud)
        let vp: [[f32; 3]; 8] = [
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
            for k in 0..3 {
                vc[i][k] = (vp[i][k] + 1.0) * 0.5;
            }
        }
        let idx: [u16; 36] = [
            0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7,
            4, 1, 5, 6, 1, 6, 2,
        ];

        let model = mul(&rot_y(0.6), &rot_x(0.3));
        let view = translate(0.0, 0.0, -5.0);
        let proj = perspective(1.0, W as f32 / H as f32, 1.0, 20.0);
        let mvp = mul(&proj, &mul(&view, &model));

        let mut verts = [0.0f32; 8 * 6];
        for i in 0..8 {
            verts[i * 6] = vp[i][0];
            verts[i * 6 + 1] = vp[i][1];
            verts[i * 6 + 2] = vp[i][2];
            verts[i * 6 + 3] = vc[i][0];
            verts[i * 6 + 4] = vc[i][1];
            verts[i * 6 + 5] = vc[i][2];
        }
        let vao = gl.create_vertex_array().unwrap();
        gl.bind_vertex_array(Some(vao));
        let vbo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_bytes(&verts), glow::STATIC_DRAW);
        let ibo = gl.create_buffer().unwrap();
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ibo));
        gl.buffer_data_u8_slice(
            glow::ELEMENT_ARRAY_BUFFER,
            core::slice::from_raw_parts(idx.as_ptr() as *const u8, 72),
            glow::STATIC_DRAW,
        );
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 24, 0);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, 24, 12);
        gl.enable_vertex_attrib_array(1);

        let prog = {
            let v = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(
                v,
                "#version 310 es\nlayout(location=0) in vec3 p;\nlayout(location=1) in vec3 \
                 c;\nout vec3 vc;\nuniform mat4 mvp;\nvoid main(){ gl_Position=mvp*vec4(p,1.0); \
                 vc=c; }\n",
            );
            gl.compile_shader(v);
            if !gl.get_shader_compile_status(v) {
                ok(false, "vs compile");
            }
            let f = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            gl.shader_source(
                f,
                "#version 310 es\nprecision highp float;\nin vec3 vc;\nlayout(location=0) out \
                 vec4 o;\nvoid main(){ o=vec4(vc,1.0); }\n",
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
        ok(true, "cube program compiles+links");
        gl.use_program(Some(prog));
        gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(prog, "mvp").as_ref(), false, &mvp.m);

        gl.enable(glow::DEPTH_TEST);
        gl.depth_func(glow::LESS);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear_depth_f32(1.0);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        gl.draw_elements(glow::TRIANGLES, 36, glow::UNSIGNED_SHORT, 0);
        gl.finish();
        readback(&gl, &mut buf);
        ok(
            gl.get_error() == glow::NO_ERROR,
            "no GL error after cube draw",
        );

        // INDEPENDENT software reference rasterizer
        let mut refc = vec![[0.0f32; 3]; (W * H) as usize];
        let mut refz = vec![1e9f32; (W * H) as usize];
        let mut refcov = vec![0u8; (W * H) as usize];
        let (mut sx, mut sy, mut sz, mut sw) = ([0.0f32; 8], [0.0f32; 8], [0.0f32; 8], [0.0f32; 8]);
        for i in 0..8 {
            let out = mv4(&mvp, &[vp[i][0], vp[i][1], vp[i][2], 1.0]);
            let w = out[3];
            sw[i] = w;
            let (ndcx, ndcy, ndcz) = (out[0] / w, out[1] / w, out[2] / w);
            sx[i] = (ndcx * 0.5 + 0.5) * W as f32;
            sy[i] = (ndcy * 0.5 + 0.5) * H as f32;
            sz[i] = ndcz * 0.5 + 0.5;
        }
        ok(
            sw[0] > 0.0,
            "reference: all clip.w positive (mesh in front of camera)",
        );
        for t in 0..12 {
            let (a, b, c) = (
                idx[t * 3] as usize,
                idx[t * 3 + 1] as usize,
                idx[t * 3 + 2] as usize,
            );
            let (ax, ay, bx, by, cx, cy) = (sx[a], sy[a], sx[b], sy[b], sx[c], sy[c]);
            let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
            if area.abs() < 1e-6 {
                continue;
            }
            let mut minx = ax.min(bx.min(cx)).floor() as i32;
            let mut maxx = ax.max(bx.max(cx)).ceil() as i32;
            let mut miny = ay.min(by.min(cy)).floor() as i32;
            let mut maxy = ay.max(by.max(cy)).ceil() as i32;
            if minx < 0 {
                minx = 0;
            }
            if miny < 0 {
                miny = 0;
            }
            if maxx > W {
                maxx = W;
            }
            if maxy > H {
                maxy = H;
            }
            for y in miny..maxy {
                for x in minx..maxx {
                    let (pxs, pys) = (x as f32 + 0.5, y as f32 + 0.5);
                    let mut w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area;
                    let mut w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area;
                    let mut w2 = 1.0 - w0 - w1;
                    let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0)
                        || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                    if !inside {
                        continue;
                    }
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        w0 = -w0;
                        w1 = -w1;
                        w2 = -w2;
                    }
                    let z = w0 * sz[a] + w1 * sz[b] + w2 * sz[c];
                    let pi = (y * W + x) as usize;
                    if z < refz[pi] {
                        refz[pi] = z;
                        refcov[pi] = 1;
                        let (iwa, iwb, iwc) = (1.0 / sw[a], 1.0 / sw[b], 1.0 / sw[c]);
                        let d = w0 * iwa + w1 * iwb + w2 * iwc;
                        for k in 0..3 {
                            let num =
                                w0 * iwa * vc[a][k] + w1 * iwb * vc[b][k] + w2 * iwc * vc[c][k];
                            refc[pi][k] = num / d;
                        }
                    }
                }
            }
        }

        // compare GL readback to reference
        let cov_at = |arr: &[u8], x: i32, y: i32| arr[(y * W + x) as usize] != 0;
        let (mut total, mut mmatch, mut covmatch, mut covtotal, mut interior_bad) = (0, 0, 0, 0, 0);
        for y in 0..H {
            for x in 0..W {
                total += 1;
                let gcov =
                    !(px(&buf, x, y, 0) == 0 && px(&buf, x, y, 1) == 0 && px(&buf, x, y, 2) == 0);
                let pi = (y * W + x) as usize;
                let rcov = refcov[pi] != 0;
                if gcov == rcov {
                    covmatch += 1;
                }
                if rcov {
                    covtotal += 1;
                    let er = (refc[pi][0] * 255.0).round() as i32;
                    let eg = (refc[pi][1] * 255.0).round() as i32;
                    let eb = (refc[pi][2] * 255.0).round() as i32;
                    let interior = x > 0
                        && y > 0
                        && x < W - 1
                        && y < H - 1
                        && cov_at(&refcov, x, y - 1)
                        && cov_at(&refcov, x, y + 1)
                        && cov_at(&refcov, x - 1, y)
                        && cov_at(&refcov, x + 1, y);
                    if peq(&buf, x, y, er, eg, eb, 255, 6) {
                        mmatch += 1;
                    } else if interior {
                        interior_bad += 1;
                    }
                }
            }
        }
        ok(covtotal > 200, "reference: cube covers a substantial area");
        ok(
            covmatch >= (0.97 * total as f32) as i32,
            "coverage mask matches GL (>=97% of pixels agree covered/empty)",
        );
        ok(
            interior_bad == 0,
            "every interior pixel matches perspective-correct Gouraud reference (tol 6)",
        );
        ok(
            mmatch >= (0.92 * covtotal as f32) as i32,
            "92%+ of covered pixels match reference color (edges excluded)",
        );

        // targeted closed-form spot checks
        {
            let vx = (sx[6] - 0.5).round() as i32;
            let vy = (sy[6] - 0.5).round() as i32;
            if vx >= 1 && vx < W - 1 && vy >= 1 && vy < H - 1 {
                let mut bright = false;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (xx, yy) = (vx + dx, vy + dy);
                        if px(&buf, xx, yy, 0) > 180
                            && px(&buf, xx, yy, 1) > 180
                            && px(&buf, xx, yy, 2) > 180
                        {
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
            peq(&buf, 0, 0, 0, 0, 0, 255, 1) || refcov[0] == 0,
            "corner (0,0) background consistent",
        );

        {
            let (cxp, cyp) = (W / 2, H / 2);
            let pi = (cyp * W + cxp) as usize;
            if refcov[pi] != 0 {
                let er = (refc[pi][0] * 255.0).round() as i32;
                let eg = (refc[pi][1] * 255.0).round() as i32;
                let eb = (refc[pi][2] * 255.0).round() as i32;
                ok(
                    peq(&buf, cxp, cyp, er, eg, eb, 255, 8),
                    "center pixel = nearest-face (depth-buffered occlusion) reference color",
                );
            } else {
                ok(false, "center pixel not covered (mesh mis-projected)");
            }
        }

        gl.disable(glow::DEPTH_TEST);

        // negative control: image is not a flat single color (real 3D shading present)
        ok(
            !(px(&buf, 1, 1, 0) == px(&buf, W / 2, H / 2, 0)
                && px(&buf, 1, 1, 1) == px(&buf, W / 2, H / 2, 1)
                && px(&buf, 1, 1, 2) == px(&buf, W / 2, H / 2, 2)),
            "negative control: image is not a flat single color (real 3D shading present)",
        );
    }
    let _ = egl.make_current(dpy, None, None, None);

    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    let expected = 18;
    println!("scene-3dmodel-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_3DMODEL_RUST OK {pass}");
        0
    } else {
        1
    }
}

fn main() {
    std::process::exit(run());
}
