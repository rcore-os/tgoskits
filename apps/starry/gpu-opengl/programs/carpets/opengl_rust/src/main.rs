// opengl_rust_full_api - desktop OpenGL 4.5 core compute carpet over surfaceless EGL, driven by
// glow + khronos-egl. Mirrors opengl_c_full_api.c but binds EGL_OPENGL_API (not ES) and requests a
// GL 4.5 CORE context. Covers the compute lifecycle: EGL display/init/config/context/make-current,
// SSBO create + immutable buffer_storage + map/unmap (read and write+flush), GLSL 430 compute-shader
// compile+link (plus a malformed-GLSL compile-failure path), vadd/saxpy dispatch, a shared-memory
// reduction, indirect dispatch, fence_sync + client_wait_sync GPU->host completion, timestamp query,
// object-validity predicates, GL-error provocation, boundary sizes (zero, non-divisible tail, >=1<<20),
// and per-operator negative controls. Prints "OPENGL_RUST_FULL_API OK <n>" on all pass.

use std::os::raw::{c_char, c_uint};

use glow::HasContext;

const GL_INVALID_INDEX: c_uint = 0xFFFF_FFFF;

type GetProgramResourceIndex =
    unsafe extern "C" fn(program: c_uint, interface: c_uint, name: *const c_char) -> c_uint;
type GetProgramInterfaceiv =
    unsafe extern "C" fn(program: c_uint, interface: c_uint, pname: c_uint, params: *mut i32);
type GetProgramResourceiv = unsafe extern "C" fn(
    program: c_uint,
    interface: c_uint,
    index: c_uint,
    prop_count: i32,
    props: *const c_uint,
    buf_size: i32,
    length: *mut i32,
    params: *mut i32,
);
type ClearBufferData = unsafe extern "C" fn(
    target: c_uint,
    internalformat: c_uint,
    format: c_uint,
    type_: c_uint,
    data: *const std::ffi::c_void,
);

fn program_raw(p: glow::Program) -> c_uint {
    p.0.get()
}

static mut PASS: i32 = 0;
static mut FAIL: i32 = 0;

fn ok(cond: bool, desc: &str) {
    unsafe {
        if cond {
            PASS += 1;
        } else {
            FAIL += 1;
            eprintln!("FAIL: {desc}");
        }
    }
}

fn feq(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * (1.0 + b.abs())
}

const CS: &str = "\
#version 430
layout(local_size_x=64) in;
layout(std430,binding=0) readonly buffer A { float a[]; };
layout(std430,binding=1) readonly buffer B { float b[]; };
layout(std430,binding=2) writeonly buffer C { float c[]; };
uniform float alpha; uniform uint n;
void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }
";

// Shared-memory (local) reduction: each 256-wide work group sums its slice into out[group].
const RED: &str = "\
#version 430
layout(local_size_x=256) in;
layout(std430,binding=0) readonly buffer In  { float src[]; };
layout(std430,binding=1) writeonly buffer Out { float dst[]; };
uniform uint n;
shared float part[256];
void main(){
  uint gid = gl_GlobalInvocationID.x;
  uint lid = gl_LocalInvocationID.x;
  part[lid] = (gid < n) ? src[gid] : 0.0;
  barrier();
  for (uint s = 128u; s > 0u; s >>= 1u) {
    if (lid < s) part[lid] += part[lid + s];
    barrier();
  }
  if (lid == 0u) dst[gl_WorkGroupID.x] = part[0];
}
";

// Deliberately malformed GLSL to exercise the compile-failure path.
const BAD_CS: &str = "\
#version 430
layout(local_size_x=64) in;
void main(){ this is not glsl @@ ; }
";

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    const N: usize = 1024;
    let bytes = N * std::mem::size_of::<f32>();

    // --- EGL surfaceless display + desktop-GL 4.5 core context ---
    let egl = unsafe {
        khronos_egl::DynamicInstance::<khronos_egl::EGL1_5>::load_required().expect("load libEGL")
    };

    let dpy = unsafe { egl.get_display(khronos_egl::DEFAULT_DISPLAY) };
    ok(dpy.is_some(), "eglGetDisplay");
    let dpy = dpy.expect("no EGL display");

    let init = egl.initialize(dpy);
    ok(init.is_ok(), "eglInitialize");
    // initialize yields the EGL major.minor; 1.5 was requested via EGL1_5, so major must be >=1.
    if let Ok((emaj, emin)) = init {
        ok(emaj >= 1 && emin >= 0, "eglInitialize version >= 1.0");
    } else {
        ok(false, "eglInitialize version >= 1.0");
    }

    let vendor = egl.query_string(Some(dpy), khronos_egl::VENDOR);
    ok(
        vendor
            .as_ref()
            .map(|s| !s.to_bytes().is_empty())
            .unwrap_or(false),
        "eglQueryString VENDOR",
    );

    // Desktop GL needs a config that advertises EGL_OPENGL_BIT (not ES).
    let cfg_attrs = [
        khronos_egl::SURFACE_TYPE,
        khronos_egl::PBUFFER_BIT,
        khronos_egl::RENDERABLE_TYPE,
        khronos_egl::OPENGL_BIT,
        khronos_egl::NONE,
    ];
    let cfg = egl.choose_first_config(dpy, &cfg_attrs);
    ok(matches!(cfg, Ok(Some(_))), "eglChooseConfig OPENGL_BIT");
    let cfg = cfg.ok().flatten().expect("no matching EGL config");
    // The chosen config must actually advertise the OPENGL renderable bit we asked for.
    {
        let rt = egl.get_config_attrib(dpy, cfg, khronos_egl::RENDERABLE_TYPE);
        let has_gl = rt
            .map(|v| (v & khronos_egl::OPENGL_BIT) != 0)
            .unwrap_or(false);
        ok(has_gl, "config RENDERABLE_TYPE has OPENGL_BIT");
    }

    // Bind the desktop OpenGL API (this is the DESKTOP-GL cell, not GLES).
    ok(
        egl.bind_api(khronos_egl::OPENGL_API).is_ok(),
        "eglBindAPI OPENGL",
    );

    // GL 4.5 CORE context: compute shaders need >= 4.3.
    let ctx_attrs = [
        khronos_egl::CONTEXT_MAJOR_VERSION,
        4,
        khronos_egl::CONTEXT_MINOR_VERSION,
        5,
        khronos_egl::CONTEXT_OPENGL_PROFILE_MASK,
        khronos_egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
        khronos_egl::NONE,
    ];
    let ctx = egl.create_context(dpy, cfg, None, &ctx_attrs);
    ok(ctx.is_ok(), "eglCreateContext 4.5 core");
    let ctx = ctx.expect("no EGL context");

    let mc = egl.make_current(dpy, None, None, Some(ctx));
    ok(mc.is_ok(), "eglMakeCurrent surfaceless");

    // glow GL context from the EGL proc loader.
    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            egl.get_proc_address(s)
                .map(|p| p as *const std::ffi::c_void)
                .unwrap_or(std::ptr::null())
        })
    };

    // glow 0.13 has no wrapper for program-resource-index / program-interface queries; load the raw
    // GL 4.3 entry points from the same EGL proc loader.
    let get_program_resource_index: GetProgramResourceIndex = unsafe {
        std::mem::transmute(
            egl.get_proc_address("glGetProgramResourceIndex")
                .expect("glGetProgramResourceIndex"),
        )
    };
    let get_program_interfaceiv: GetProgramInterfaceiv = unsafe {
        std::mem::transmute(
            egl.get_proc_address("glGetProgramInterfaceiv")
                .expect("glGetProgramInterfaceiv"),
        )
    };
    // glow 0.13's get_program_resource_i32 first-queries with bufSize=0, so it returns an empty Vec;
    // call the raw entry point to actually read the resource property.
    let get_program_resourceiv: GetProgramResourceiv = unsafe {
        std::mem::transmute(
            egl.get_proc_address("glGetProgramResourceiv")
                .expect("glGetProgramResourceiv"),
        )
    };
    // glow 0.13 does not wrap glClearBufferData (its clear_buffer_*_slice are framebuffer clears);
    // load the raw GL 4.3 entry point.
    let clear_buffer_data: ClearBufferData = unsafe {
        std::mem::transmute(
            egl.get_proc_address("glClearBufferData")
                .expect("glClearBufferData"),
        )
    };

    unsafe {
        let ver = gl.get_parameter_string(glow::VERSION);
        ok(!ver.is_empty(), "GL_VERSION non-empty");
        // Desktop GL reports "4.5 ..." (no "ES"); assert 4.x >= 4.3 and no ES marker.
        ok(!ver.contains("ES"), "GL_VERSION is desktop (no ES)");
        ok(gl_at_least_4_3(&ver), "GL_VERSION >= 4.3");
        let glsl = gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION);
        ok(!glsl.is_empty(), "GL_SHADING_LANGUAGE_VERSION");
        let renderer = gl.get_parameter_string(glow::RENDERER);
        ok(!renderer.is_empty(), "GL_RENDERER");
        let vnd = gl.get_parameter_string(glow::VENDOR);
        ok(!vnd.is_empty(), "GL_VENDOR");

        // --- compute limits ---
        let wgc = gl.get_parameter_indexed_i32(glow::MAX_COMPUTE_WORK_GROUP_COUNT, 0);
        ok(wgc >= 1, "MAX_COMPUTE_WORK_GROUP_COUNT[0]");
        let wgs = gl.get_parameter_indexed_i32(glow::MAX_COMPUTE_WORK_GROUP_SIZE, 0);
        ok(wgs >= 64, "MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 64");
        let wgi = gl.get_parameter_i32(glow::MAX_COMPUTE_WORK_GROUP_INVOCATIONS);
        ok(wgi >= 64, "MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64");
        let csb = gl.get_parameter_i32(glow::MAX_COMPUTE_SHADER_STORAGE_BLOCKS);
        ok(csb >= 3, "MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3");
        // The reduction shader declares 256-wide local groups; the impl must permit that.
        ok(
            wgi >= 256,
            "MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 256 (reduction)",
        );

        // --- shader + program ---
        let sh = gl.create_shader(glow::COMPUTE_SHADER);
        ok(sh.is_ok(), "glCreateShader(COMPUTE)");
        let sh = sh.expect("create_shader");
        ok(gl.is_shader(sh), "glIsShader after create == true");
        gl.shader_source(sh, CS);
        gl.compile_shader(sh);
        let cok = gl.get_shader_compile_status(sh);
        if !cok {
            eprintln!("shader: {}", gl.get_shader_info_log(sh));
        }
        ok(cok, "glCompileShader COMPILE_STATUS");

        // Negative path: a malformed shader must FAIL to compile and emit a non-empty info log.
        let bad = gl.create_shader(glow::COMPUTE_SHADER).expect("bad shader");
        gl.shader_source(bad, BAD_CS);
        gl.compile_shader(bad);
        let bad_status = gl.get_shader_compile_status(bad);
        ok(!bad_status, "malformed GLSL COMPILE_STATUS == false");
        ok(
            !gl.get_shader_info_log(bad).is_empty(),
            "malformed GLSL info log non-empty",
        );
        // `bad` is not attached to any program, so delete really frees it: IsShader must go false.
        gl.delete_shader(bad);
        ok(
            !gl.is_shader(bad),
            "glIsShader after delete (unattached) == false",
        );

        let prog = gl.create_program().expect("create_program");
        ok(gl.is_program(prog), "glIsProgram after create == true");
        gl.attach_shader(prog, sh);
        gl.link_program(prog);
        let lok = gl.get_program_link_status(prog);
        if !lok {
            eprintln!("program: {}", gl.get_program_info_log(prog));
        }
        ok(lok, "glLinkProgram LINK_STATUS");
        // sh is still attached to prog, so delete only flags it (IsShader stays true until detach).
        // Detach then confirm the flagged shader is actually gone.
        gl.detach_shader(prog, sh);
        gl.delete_shader(sh);
        ok(!gl.is_shader(sh), "glIsShader after detach+delete == false");

        // --- SSBO buffers ---
        let mut a = vec![0f32; N];
        let mut b = vec![0f32; N];
        for i in 0..N {
            a[i] = i as f32;
            b[i] = 2.0 * i as f32 + 1.0;
        }
        let buf0 = gl.create_buffer().expect("buf0");
        let buf1 = gl.create_buffer().expect("buf1");
        let buf2 = gl.create_buffer().expect("buf2");
        // create_buffer only registers a name; is_buffer is true only after first bind.
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        gl.buffer_data_u8_slice(glow::SHADER_STORAGE_BUFFER, as_bytes(&a), glow::STATIC_DRAW);
        ok(
            gl.is_buffer(buf0),
            "glIsBuffer buf0 after bind+data == true",
        );
        {
            let sz = gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE);
            ok(sz == bytes as i32, "buf0 BUFFER_SIZE == bytes");
        }
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf1));
        gl.buffer_data_u8_slice(glow::SHADER_STORAGE_BUFFER, as_bytes(&b), glow::STATIC_DRAW);
        ok(
            gl.is_buffer(buf1),
            "glIsBuffer buf1 after bind+data == true",
        );
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        gl.buffer_data_size(
            glow::SHADER_STORAGE_BUFFER,
            bytes as i32,
            glow::DYNAMIC_COPY,
        );
        {
            let sz = gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE);
            ok(sz == bytes as i32, "buf2 BUFFER_SIZE == bytes");
        }

        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));
        {
            // SHADER_STORAGE_BUFFER_BINDING at index 2 must reflect buf2's name.
            let bound = gl.get_parameter_indexed_i32(glow::SHADER_STORAGE_BUFFER_BINDING, 2);
            ok(bound as u32 == buf2.0.get(), "SSBO binding[2] == buf2");
        }

        // --- uniforms + dispatch: vadd (alpha=1); read back via glGetBufferSubData ---
        gl.use_program(Some(prog));
        {
            let cur = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
            ok(cur as u32 == prog.0.get(), "CURRENT_PROGRAM == prog");
        }
        let la = gl.get_uniform_location(prog, "alpha");
        let ln = gl.get_uniform_location(prog, "n");
        ok(la.is_some() && ln.is_some(), "glGetUniformLocation alpha/n");
        let la = la.expect("alpha loc");
        let ln = ln.expect("n loc");
        gl.uniform_1_f32(Some(&la), 1.0);
        gl.uniform_1_u32(Some(&ln), N as u32);
        {
            let mut read = [0.0f32; 1];
            gl.get_uniform_f32(prog, &la, &mut read);
            ok(feq(read[0], 1.0), "glGetUniform alpha read-back == 1.0");
        }
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);

        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], a[i] + b[i]));
            ok(g, "vadd == a+b (glGetBufferSubData)");
            // Negative control: the checker must reject a corrupted device output.
            let mut bad = hc.clone();
            bad[7] += 1.0;
            ok(
                !(0..N).all(|i| feq(bad[i], a[i] + b[i])),
                "vadd negative control detects corruption",
            );
        }

        // --- fence sync around a saxpy dispatch: GPU->host completion, not just a memory barrier ---
        gl.uniform_1_f32(Some(&la), 3.0);
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
        let fence = gl
            .fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0)
            .expect("fence_sync");
        ok(gl.is_sync(fence), "glIsSync after fence_sync == true");
        let waited = gl.client_wait_sync(fence, glow::SYNC_FLUSH_COMMANDS_BIT, 1_000_000_000);
        ok(
            waited == glow::ALREADY_SIGNALED || waited == glow::CONDITION_SATISFIED,
            "client_wait_sync signaled (not TIMEOUT/WAIT_FAILED)",
        );
        let status = gl.get_sync_status(fence);
        ok(status == glow::SIGNALED, "get_sync_status == SIGNALED");
        gl.delete_sync(fence);
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], 3.0 * a[i] + b[i]));
            ok(g, "saxpy == 3*a+b (after fence wait)");
        }

        // --- map buffer range (read) confirms saxpy result too ---
        let m = map_read(&gl, N);
        ok(m.is_some(), "glMapBufferRange READ");
        {
            let g = m
                .as_ref()
                .map(|v| feq(v[0], b[0]) && (0..N).all(|i| feq(v[i], 3.0 * a[i] + b[i])))
                .unwrap_or(false);
            ok(g, "mapped range == 3*a+b (saxpy)");
        }
        gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);

        // --- write-through map + flush: host->device visibility, then re-dispatch reads it back ---
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        let wrote = map_write_fill(&gl, N, 4.0);
        ok(wrote, "glMapBufferRange WRITE+flush filled A<-4");
        gl.uniform_1_f32(Some(&la), 1.0);
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], 4.0 + b[i]));
            ok(g, "vadd after write-map == 4+b");
        }

        // --- buffer sub-data update + re-dispatch determinism ---
        let a2 = vec![2.0f32; N];
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&a2));
        gl.uniform_1_f32(Some(&la), 1.0);
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], 2.0 + b[i]));
            ok(g, "vadd after subdata == 2+b");
        }

        // --- indirect dispatch: same vadd via glDispatchComputeIndirect reading group counts from a
        //     DISPATCH_INDIRECT_BUFFER ---
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&a));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));
        gl.uniform_1_f32(Some(&la), 1.0);
        let indirect = gl.create_buffer().expect("indirect buf");
        let groups: [u32; 3] = [((N + 63) / 64) as u32, 1, 1];
        gl.bind_buffer(glow::DISPATCH_INDIRECT_BUFFER, Some(indirect));
        gl.buffer_data_u8_slice(
            glow::DISPATCH_INDIRECT_BUFFER,
            as_bytes_u32(&groups),
            glow::STATIC_DRAW,
        );
        gl.dispatch_compute_indirect(0);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT | glow::COMMAND_BARRIER_BIT);
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], a[i] + b[i]));
            ok(g, "indirect vadd == a+b");
            let mut bad = hc.clone();
            bad[0] = -1.0;
            ok(
                !(0..N).all(|i| feq(bad[i], a[i] + b[i])),
                "indirect vadd negative control detects corruption",
            );
        }
        gl.delete_buffer(indirect);

        // --- timestamp query: two counters bracket a dispatch; both must be AVAILABLE and monotone ---
        {
            let q0 = gl.create_query().expect("q0");
            let q1 = gl.create_query().expect("q1");
            gl.query_counter(q0, glow::TIMESTAMP);
            gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            gl.query_counter(q1, glow::TIMESTAMP);
            gl.finish();
            let avail = gl.get_query_parameter_u32(q1, glow::QUERY_RESULT_AVAILABLE);
            ok(
                avail == glow::TRUE as u32,
                "timestamp query RESULT_AVAILABLE",
            );
            let t0 = gl.get_query_parameter_u32(q0, glow::QUERY_RESULT);
            let t1 = gl.get_query_parameter_u32(q1, glow::QUERY_RESULT);
            ok(t1 >= t0, "timestamp query t1 >= t0 (monotone)");
            gl.delete_query(q0);
            gl.delete_query(q1);
        }

        // --- shared-memory reduction: sum src via a 256-wide group reduction, then finish on CPU ---
        {
            const RN: usize = 4096;
            let groups = RN / 256; // 16
            let src: Vec<f32> = (0..RN).map(|i| (i % 7) as f32 + 1.0).collect();
            let expect_sum: f32 = src.iter().sum();

            let rsh = gl.create_shader(glow::COMPUTE_SHADER).expect("red shader");
            gl.shader_source(rsh, RED);
            gl.compile_shader(rsh);
            ok(
                gl.get_shader_compile_status(rsh),
                "reduction COMPILE_STATUS",
            );
            let rprog = gl.create_program().expect("red prog");
            gl.attach_shader(rprog, rsh);
            gl.link_program(rprog);
            ok(gl.get_program_link_status(rprog), "reduction LINK_STATUS");
            gl.delete_shader(rsh);

            let rin = gl.create_buffer().expect("rin");
            let rout = gl.create_buffer().expect("rout");
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(rin));
            gl.buffer_data_u8_slice(
                glow::SHADER_STORAGE_BUFFER,
                as_bytes(&src),
                glow::STATIC_DRAW,
            );
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(rout));
            gl.buffer_data_size(
                glow::SHADER_STORAGE_BUFFER,
                (groups * 4) as i32,
                glow::DYNAMIC_COPY,
            );
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(rin));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(rout));
            gl.use_program(Some(rprog));
            let rn = gl.get_uniform_location(rprog, "n").expect("red n");
            gl.uniform_1_u32(Some(&rn), RN as u32);
            gl.dispatch_compute(groups as u32, 1, 1);
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);

            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(rout));
            let partials = get_sub_data(&gl, groups);
            let total: f32 = partials.iter().sum();
            ok(feq(total, expect_sum), "reduction sum == closed form");
            // Negative control: corrupt a partial and confirm the sum check rejects it.
            let mut badp = partials.clone();
            badp[0] += 100.0;
            let bad_total: f32 = badp.iter().sum();
            ok(
                !feq(bad_total, expect_sum),
                "reduction negative control detects corruption",
            );
            gl.delete_buffer(rin);
            gl.delete_buffer(rout);
            gl.delete_program(rprog);
            gl.use_program(Some(prog));
        }

        // === boundary cases ===
        // Zero-length dispatch: n=0 leaves buf2 unchanged from a known sentinel fill.
        {
            let sentinel = vec![-9.0f32; N];
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
            gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&sentinel));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));
            gl.uniform_1_u32(Some(&ln), 0);
            gl.dispatch_compute(0, 1, 1);
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            let hc = get_sub_data(&gl, N);
            ok(
                (0..N).all(|i| feq(hc[i], -9.0)),
                "zero-length dispatch leaves buffer unchanged",
            );
            gl.uniform_1_u32(Some(&ln), N as u32);
        }

        // Non-divisible tail: N2=1000 is not a multiple of 64; the shader's if(i<n) guard must stop
        // element 1000..1023 (last group's overflow lanes) from writing past the logical range.
        {
            const N2: usize = 1000;
            let sentinel = vec![7.0f32; N];
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
            gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&sentinel));
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
            gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&a));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));
            gl.uniform_1_f32(Some(&la), 1.0);
            gl.uniform_1_u32(Some(&ln), N2 as u32);
            gl.dispatch_compute(((N2 + 63) / 64) as u32, 1, 1);
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            let hc = get_sub_data(&gl, N);
            let head = (0..N2).all(|i| feq(hc[i], a[i] + b[i]));
            let tail = (N2..N).all(|i| feq(hc[i], 7.0));
            ok(head, "non-divisible head [0,1000) == a+b");
            ok(
                tail,
                "non-divisible tail [1000,1024) untouched (guard held)",
            );
            gl.uniform_1_u32(Some(&ln), N as u32);
        }

        // Large case: N3 >= 1<<20 saxpy, checked element-wise vs closed form on a strided sample.
        {
            const N3: usize = 1 << 20;
            let big_a: Vec<f32> = (0..N3).map(|i| (i % 251) as f32).collect();
            let big_b: Vec<f32> = (0..N3).map(|i| ((i % 97) as f32) * 0.5).collect();
            let big0 = gl.create_buffer().expect("big0");
            let big1 = gl.create_buffer().expect("big1");
            let big2 = gl.create_buffer().expect("big2");
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big0));
            gl.buffer_data_u8_slice(
                glow::SHADER_STORAGE_BUFFER,
                as_bytes(&big_a),
                glow::STATIC_DRAW,
            );
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big1));
            gl.buffer_data_u8_slice(
                glow::SHADER_STORAGE_BUFFER,
                as_bytes(&big_b),
                glow::STATIC_DRAW,
            );
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big2));
            gl.buffer_data_size(
                glow::SHADER_STORAGE_BUFFER,
                (N3 * 4) as i32,
                glow::DYNAMIC_COPY,
            );
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(big0));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(big1));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(big2));
            gl.uniform_1_f32(Some(&la), 3.0);
            gl.uniform_1_u32(Some(&ln), N3 as u32);
            gl.dispatch_compute(((N3 + 63) / 64) as u32, 1, 1);
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big2));
            let hc = get_sub_data(&gl, N3);
            // element-wise across the full range via a stride to bound wall time
            let mut good = true;
            let mut i = 0;
            while i < N3 {
                if !feq(hc[i], 3.0 * big_a[i] + big_b[i]) {
                    good = false;
                    break;
                }
                i += 97;
            }
            // and the exact boundary elements
            good &= feq(hc[0], 3.0 * big_a[0] + big_b[0]);
            good &= feq(hc[N3 - 1], 3.0 * big_a[N3 - 1] + big_b[N3 - 1]);
            ok(good, "large 1<<20 saxpy == 3*a+b element-wise");
            let mut bad = hc.clone();
            bad[N3 / 2] = f32::NAN;
            ok(
                !feq(bad[N3 / 2], 3.0 * big_a[N3 / 2] + big_b[N3 / 2]),
                "large saxpy negative control detects corruption",
            );
            gl.delete_buffer(big0);
            gl.delete_buffer(big1);
            gl.delete_buffer(big2);
            gl.uniform_1_u32(Some(&ln), N as u32);
        }

        // restore working set bindings for the introspection tail
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&a2));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));

        // === extended coverage: copy-sub-data / clear-buffer-data / bind-buffer-range /
        //     immutable buffer_storage / SSBO block binding / resource introspection ===
        gl.uniform_1_f32(Some(&la), 1.0);
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);

        gl.bind_buffer(glow::COPY_READ_BUFFER, Some(buf0));
        gl.bind_buffer(glow::COPY_WRITE_BUFFER, Some(buf2));
        gl.copy_buffer_sub_data(
            glow::COPY_READ_BUFFER,
            glow::COPY_WRITE_BUFFER,
            0,
            0,
            bytes as i32,
        );
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], 2.0));
            ok(g, "copy-sub-data buf0(=2)->buf2");
        }

        // glClearBufferData is desktop GL 4.3 (raw entry point loaded above).
        let cv: f32 = 5.0;
        clear_buffer_data(
            glow::SHADER_STORAGE_BUFFER,
            glow::R32F,
            glow::RED,
            glow::FLOAT,
            &cv as *const f32 as *const std::ffi::c_void,
        );
        {
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], 5.0));
            ok(g, "clear-buffer-data == 5.0");
            let mut bad = hc.clone();
            bad[3] = 0.0;
            ok(
                !(0..N).all(|i| feq(bad[i], 5.0)),
                "clear negative control detects corruption",
            );
        }

        // Immutable storage: allocate with buffer_storage + DYNAMIC_STORAGE_BIT, fill, dispatch reads.
        {
            let imm = gl.create_buffer().expect("imm");
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(imm));
            gl.buffer_storage(
                glow::SHADER_STORAGE_BUFFER,
                bytes as i32,
                Some(as_bytes(&a)),
                glow::DYNAMIC_STORAGE_BIT | glow::MAP_READ_BIT,
            );
            let sz = gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE);
            ok(sz == bytes as i32, "buffer_storage BUFFER_SIZE == bytes");
            let imm_flag = gl.get_buffer_parameter_i32(
                glow::SHADER_STORAGE_BUFFER,
                glow::BUFFER_IMMUTABLE_STORAGE,
            );
            ok(
                imm_flag == glow::TRUE as i32,
                "BUFFER_IMMUTABLE_STORAGE == true",
            );
            // use imm as input A, dispatch vadd, verify a+b
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(imm));
            gl.uniform_1_f32(Some(&la), 1.0);
            gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
            let hc = get_sub_data(&gl, N);
            let g = (0..N).all(|i| feq(hc[i], a[i] + b[i]));
            ok(g, "buffer_storage input vadd == a+b");
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
            gl.delete_buffer(imm);
        }

        gl.bind_buffer_range(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0), 0, bytes as i32);
        {
            let bound = gl.get_parameter_indexed_i32(glow::SHADER_STORAGE_BUFFER_BINDING, 0);
            ok(
                bound as u32 == buf0.0.get(),
                "bind_buffer_range binding[0] == buf0",
            );
        }

        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        let bsz = gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE);
        ok(bsz == bytes as i32, "glGetBufferParameteriv BUFFER_SIZE");

        let prog_raw = program_raw(prog);
        let name_a = std::ffi::CString::new("A").unwrap();
        let idx = get_program_resource_index(prog_raw, glow::SHADER_STORAGE_BLOCK, name_a.as_ptr());
        ok(idx != GL_INVALID_INDEX, "glGetProgramResourceIndex A");

        // glShaderStorageBlockBinding (desktop GL 4.3), wrapped by glow.
        if idx != GL_INVALID_INDEX {
            gl.shader_storage_block_binding(prog, idx, 0);
        }

        let mut nres: i32 = 0;
        get_program_interfaceiv(
            prog_raw,
            glow::SHADER_STORAGE_BLOCK,
            glow::ACTIVE_RESOURCES,
            &mut nres,
        );
        ok(nres >= 3, "glGetProgramInterfaceiv ACTIVE_RESOURCES(>=3)");

        {
            let mut binding: i32 = -1;
            let props = [glow::BUFFER_BINDING];
            if idx != GL_INVALID_INDEX {
                get_program_resourceiv(
                    prog_raw,
                    glow::SHADER_STORAGE_BLOCK,
                    idx,
                    props.len() as i32,
                    props.as_ptr(),
                    1,
                    std::ptr::null_mut(),
                    &mut binding,
                );
            }
            ok(binding == 0, "glGetProgramResourceiv BUFFER_BINDING(A)==0");
        }

        // Everything above should be error-clean.
        ok(
            gl.get_error() == glow::NO_ERROR,
            "glGetError == GL_NO_ERROR (clean)",
        );

        // Error-provocation: an unsupported clear-buffer format combination must set INVALID_*.
        // glBindBufferBase with a bogus index >= MAX_SHADER_STORAGE_BUFFER_BINDINGS is INVALID_VALUE.
        {
            let max_ssbo = gl.get_parameter_i32(glow::MAX_SHADER_STORAGE_BUFFER_BINDINGS);
            gl.bind_buffer_base(
                glow::SHADER_STORAGE_BUFFER,
                (max_ssbo as u32).wrapping_add(1000),
                Some(buf0),
            );
            let e = gl.get_error();
            ok(
                e == glow::INVALID_VALUE,
                "bad bind_buffer_base index -> INVALID_VALUE",
            );
            ok(
                gl.get_error() == glow::NO_ERROR,
                "error state cleared after read",
            );
        }

        // --- cleanup ---
        gl.delete_buffer(buf0);
        gl.delete_buffer(buf1);
        gl.delete_buffer(buf2);
        // Unbind first: a program that is still current is only flagged, not freed.
        gl.use_program(None);
        gl.delete_program(prog);
        ok(
            !gl.is_program(prog),
            "glIsProgram after unbind+delete == false",
        );
    }

    let _ = egl.make_current(dpy, None, None, None);
    ok(egl.destroy_context(dpy, ctx).is_ok(), "eglDestroyContext");
    ok(egl.terminate(dpy).is_ok(), "eglTerminate");

    let expected = 78;
    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    println!("opengl-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("OPENGL_RUST_FULL_API OK {pass}");
        0
    } else {
        println!("OPENGL_RUST_FULL_API FAIL");
        1
    }
}

fn as_bytes(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn as_bytes_u32(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn gl_at_least_4_3(ver: &str) -> bool {
    // GL_VERSION starts with "<major>.<minor>" possibly followed by vendor text.
    let head = ver.split_whitespace().next().unwrap_or("");
    let mut it = head.split('.');
    let major: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    major > 4 || (major == 4 && minor >= 3)
}

// Desktop GL has glGetBufferSubData (unlike GLES); read N f32s from the currently bound SSBO.
unsafe fn get_sub_data(gl: &glow::Context, n: usize) -> Vec<f32> {
    let bytes = n * std::mem::size_of::<f32>();
    let mut raw = vec![0u8; bytes];
    gl.get_buffer_sub_data(glow::SHADER_STORAGE_BUFFER, 0, &mut raw);
    let mut out = vec![0f32; n];
    for (i, v) in out.iter_mut().enumerate() {
        let mut b = [0u8; 4];
        b.copy_from_slice(&raw[i * 4..i * 4 + 4]);
        *v = f32::from_ne_bytes(b);
    }
    out
}

unsafe fn map_read(gl: &glow::Context, n: usize) -> Option<Vec<f32>> {
    let bytes = n * std::mem::size_of::<f32>();
    let ptr = gl.map_buffer_range(
        glow::SHADER_STORAGE_BUFFER,
        0,
        bytes as i32,
        glow::MAP_READ_BIT,
    );
    if ptr.is_null() {
        return None;
    }
    let src = std::slice::from_raw_parts(ptr as *const f32, n);
    Some(src.to_vec())
}

// Write-through mapping: map with MAP_WRITE_BIT | MAP_FLUSH_EXPLICIT_BIT, fill every element with
// `val`, flush the range, then unmap. Returns false if the mapping failed.
unsafe fn map_write_fill(gl: &glow::Context, n: usize, val: f32) -> bool {
    let bytes = n * std::mem::size_of::<f32>();
    let ptr = gl.map_buffer_range(
        glow::SHADER_STORAGE_BUFFER,
        0,
        bytes as i32,
        glow::MAP_WRITE_BIT | glow::MAP_FLUSH_EXPLICIT_BIT,
    );
    if ptr.is_null() {
        return false;
    }
    let dst = std::slice::from_raw_parts_mut(ptr as *mut f32, n);
    for x in dst.iter_mut() {
        *x = val;
    }
    gl.flush_mapped_buffer_range(glow::SHADER_STORAGE_BUFFER, 0, bytes as i32);
    gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
    true
}
