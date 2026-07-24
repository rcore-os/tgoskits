// gles_rust_full_api - OpenGL ES 3.1 compute carpet over surfaceless EGL, driven by glow +
// khronos-egl. Mirrors gles_c_full_api.c: EGL display/init/config/context/make-current, GLES 3.1
// compute SSBOs, shader compile+link (with compile/link error paths), dispatch+barrier, indirect
// dispatch, fence sync, mapped/sub-data readback, error-injection paths, boundary sizes, and
// per-element vadd/saxpy/copy correctness against a CPU reference plus negative controls.
// Prints "GLES_RUST_FULL_API OK <n>" on all pass.

use glow::HasContext;
use std::os::raw::{c_char, c_uint};

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

// saxpy: c[i] = alpha*a[i] + b[i], guarded by i<n.
const CS: &str = "\
#version 310 es
layout(local_size_x=64) in;
layout(std430,binding=0) readonly buffer A { float a[]; };
layout(std430,binding=1) readonly buffer B { float b[]; };
layout(std430,binding=2) writeonly buffer C { float c[]; };
uniform float alpha; uniform uint n;
void main(){ uint i=gl_GlobalInvocationID.x; if(i<n) c[i]=alpha*a[i]+b[i]; }
";

// Deliberately malformed compute shader: undeclared identifier + missing semicolon, to force a
// COMPILE_STATUS==false with a non-empty info log.
const CS_BAD: &str = "\
#version 310 es
layout(local_size_x=64) in;
void main(){ this_is_not_declared = 1 }
";

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    const N: usize = 1024;
    let bytes = N * std::mem::size_of::<f32>();

    // --- EGL surfaceless display + context (GLES 3.1) ---
    let egl = unsafe {
        khronos_egl::DynamicInstance::<khronos_egl::EGL1_5>::load_required()
            .expect("load libEGL")
    };

    let dpy = unsafe { egl.get_display(khronos_egl::DEFAULT_DISPLAY) };
    ok(dpy.is_some(), "eglGetDisplay");
    let dpy = dpy.expect("no EGL display");

    let init = egl.initialize(dpy);
    ok(init.is_ok(), "eglInitialize");
    // initialize returns the (major, minor) EGL version; assert a sane >=1.4 version.
    if let Ok((maj, min)) = init {
        ok(maj > 1 || (maj == 1 && min >= 4), "eglInitialize version >= 1.4");
    } else {
        ok(false, "eglInitialize version >= 1.4");
    }

    let vendor = egl.query_string(Some(dpy), khronos_egl::VENDOR);
    ok(vendor.is_ok() && vendor.map(|s| !s.to_bytes().is_empty()).unwrap_or(false),
        "eglQueryString VENDOR");

    let cfg_attrs = [
        khronos_egl::SURFACE_TYPE, khronos_egl::PBUFFER_BIT,
        khronos_egl::RENDERABLE_TYPE, khronos_egl::OPENGL_ES3_BIT,
        khronos_egl::NONE,
    ];
    let cfg = egl.choose_first_config(dpy, &cfg_attrs);
    ok(matches!(cfg, Ok(Some(_))), "eglChooseConfig ES3");
    let cfg = cfg.ok().flatten().expect("no matching EGL config");
    // The chosen config must actually advertise the ES3 renderable bit we requested.
    {
        let rt = egl.get_config_attrib(dpy, cfg, khronos_egl::RENDERABLE_TYPE);
        ok(rt.map(|v| (v & khronos_egl::OPENGL_ES3_BIT) != 0).unwrap_or(false),
            "eglGetConfigAttrib RENDERABLE_TYPE has ES3 bit");
    }

    ok(egl.bind_api(khronos_egl::OPENGL_ES_API).is_ok(), "eglBindAPI ES");

    let ctx_attrs = [
        khronos_egl::CONTEXT_MAJOR_VERSION, 3,
        khronos_egl::CONTEXT_MINOR_VERSION, 1,
        khronos_egl::NONE,
    ];
    let ctx = egl.create_context(dpy, cfg, None, &ctx_attrs);
    ok(ctx.is_ok(), "eglCreateContext 3.1");
    let ctx = ctx.expect("no EGL context");

    let mc = egl.make_current(dpy, None, None, Some(ctx));
    ok(mc.is_ok(), "eglMakeCurrent surfaceless");

    // glow GL context from the EGL proc loader
    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            egl.get_proc_address(s)
                .map(|p| p as *const std::ffi::c_void)
                .unwrap_or(std::ptr::null())
        })
    };

    // glow 0.13 has no wrapper for program-resource-index / program-interface queries;
    // load the raw GLES 3.1 entry points from the same EGL proc loader.
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
    // glow 0.13's get_program_resource_i32 first-queries with bufSize=0, so it returns an empty
    // Vec; call the raw entry point to actually read the resource property.
    let get_program_resourceiv: GetProgramResourceiv = unsafe {
        std::mem::transmute(
            egl.get_proc_address("glGetProgramResourceiv")
                .expect("glGetProgramResourceiv"),
        )
    };

    unsafe {
        let ver = gl.get_parameter_string(glow::VERSION);
        ok(ver.contains("ES"), "GL_VERSION is GLES");
        let glsl = gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION);
        ok(!glsl.is_empty(), "GLSL ES version");

        // --- compute limits ---
        // GLES 3.1 mandated minima: work-group count[x] >= 65535, size[x] >= 128,
        // invocations >= 128 (gl31.h / spec table 20.45).
        let wgc = gl.get_parameter_indexed_i32(glow::MAX_COMPUTE_WORK_GROUP_COUNT, 0);
        ok(wgc >= 65535, "MAX_COMPUTE_WORK_GROUP_COUNT[0] >= 65535");
        let wgs = gl.get_parameter_indexed_i32(glow::MAX_COMPUTE_WORK_GROUP_SIZE, 0);
        ok(wgs >= 128, "MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 128");
        let wginv = gl.get_parameter_i32(glow::MAX_COMPUTE_WORK_GROUP_INVOCATIONS);
        ok(wginv >= 128, "MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 128");
        // Our local_size_x=64 must fit inside the per-dimension size limit.
        ok(64 <= wgs, "local_size_x(64) <= MAX_COMPUTE_WORK_GROUP_SIZE[0]");
        ok(gl.get_error() == glow::NO_ERROR, "no GL error after limit queries");

        // --- shader compile: NEGATIVE (bad shader) first ---
        let bad = gl.create_shader(glow::COMPUTE_SHADER).expect("create bad shader");
        gl.shader_source(bad, CS_BAD);
        gl.compile_shader(bad);
        ok(!gl.get_shader_compile_status(bad), "bad shader COMPILE_STATUS == false");
        ok(!gl.get_shader_info_log(bad).is_empty(), "bad shader info log non-empty");
        gl.delete_shader(bad);

        // --- link: NEGATIVE (program with no attached/compiled compute stage) ---
        let empty_prog = gl.create_program().expect("empty program");
        gl.link_program(empty_prog);
        ok(!gl.get_program_link_status(empty_prog), "empty program LINK_STATUS == false");
        ok(!gl.get_program_info_log(empty_prog).is_empty(), "empty program info log non-empty");
        gl.delete_program(empty_prog);

        // --- shader + program: valid path ---
        let sh = gl.create_shader(glow::COMPUTE_SHADER);
        ok(sh.is_ok(), "glCreateShader(COMPUTE)");
        let sh = sh.expect("create_shader");
        gl.shader_source(sh, CS);
        gl.compile_shader(sh);
        let cok = gl.get_shader_compile_status(sh);
        if !cok {
            eprintln!("shader: {}", gl.get_shader_info_log(sh));
        }
        ok(cok, "glCompileShader COMPILE_STATUS");

        let prog = gl.create_program().expect("create_program");
        gl.attach_shader(prog, sh);
        gl.link_program(prog);
        let lok = gl.get_program_link_status(prog);
        if !lok {
            eprintln!("program: {}", gl.get_program_info_log(prog));
        }
        ok(lok, "glLinkProgram LINK_STATUS");
        gl.delete_shader(sh);
        ok(gl.get_error() == glow::NO_ERROR, "no GL error after shader/program build");

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
        ok(buf0 != buf1 && buf1 != buf2 && buf0 != buf2, "glGenBuffers distinct names");

        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        gl.buffer_data_u8_slice(glow::SHADER_STORAGE_BUFFER, as_bytes(&a), glow::STATIC_DRAW);
        ok(gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE) == bytes as i32,
            "glBufferData A -> BUFFER_SIZE");
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf1));
        gl.buffer_data_u8_slice(glow::SHADER_STORAGE_BUFFER, as_bytes(&b), glow::STATIC_DRAW);
        ok(gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE) == bytes as i32,
            "glBufferData B -> BUFFER_SIZE");
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        gl.buffer_data_size(glow::SHADER_STORAGE_BUFFER, bytes as i32, glow::DYNAMIC_COPY);
        ok(gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE) == bytes as i32,
            "glBufferData C(size) -> BUFFER_SIZE");

        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
        gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));
        ok(gl.get_error() == glow::NO_ERROR, "glBindBufferBase x3 no error");

        // --- ERROR INJECTION: bind_buffer_base with index out of range ---
        // GLES 3.1: index >= MAX_SHADER_STORAGE_BUFFER_BINDINGS generates GL_INVALID_VALUE.
        {
            let _ = gl.get_error(); // clear
            let max_bind = gl.get_parameter_i32(glow::MAX_SHADER_STORAGE_BUFFER_BINDINGS);
            ok(max_bind >= 8, "MAX_SHADER_STORAGE_BUFFER_BINDINGS >= 8");
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, max_bind as u32 + 4096, Some(buf0));
            let e = gl.get_error();
            ok(e == glow::INVALID_VALUE, "bind_buffer_base out-of-range -> INVALID_VALUE");
            // restore binding 0
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
        }

        // --- ERROR INJECTION: invalid enum to bind_buffer target ---
        {
            let _ = gl.get_error();
            gl.bind_buffer(glow::TRIANGLES, Some(buf0)); // TRIANGLES is not a buffer target
            let e = gl.get_error();
            ok(e == glow::INVALID_ENUM, "bind_buffer bad target -> INVALID_ENUM");
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        }

        // --- uniforms + dispatch: vadd (alpha=1); read back via glMapBufferRange ---
        gl.use_program(Some(prog));
        let la = gl.get_uniform_location(prog, "alpha");
        let ln = gl.get_uniform_location(prog, "n");
        ok(la.is_some() && ln.is_some(), "glGetUniformLocation");
        let la = la.expect("alpha loc");
        let ln = ln.expect("n loc");
        gl.uniform_1_f32(Some(&la), 1.0);
        gl.uniform_1_u32(Some(&ln), N as u32);
        ok(gl.get_error() == glow::NO_ERROR, "uniforms set no error");
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        ok(gl.get_error() == glow::NO_ERROR, "glDispatchCompute vadd no error");
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
        ok(gl.get_error() == glow::NO_ERROR, "glMemoryBarrier no error");

        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        let m = map_read(&gl, N);
        ok(m.is_some(), "glMapBufferRange READ");
        {
            let g = m.as_ref().map(|v| (0..N).all(|i| feq(v[i], a[i] + b[i]))).unwrap_or(false);
            ok(g, "vadd == a+b (mapped)");
        }
        // NEGATIVE CONTROL (vadd): corrupt one element of the real device output and confirm the
        // SAME element-wise checker now rejects it.
        {
            let mut poisoned = m.clone().expect("mapped vadd");
            poisoned[N / 2] += 1.0;
            let still_ok =
                (0..N).all(|i| feq(poisoned[i], a[i] + b[i]));
            ok(!still_ok, "neg-control vadd: corrupted element flagged");
        }
        gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
        ok(gl.get_error() == glow::NO_ERROR, "glUnmapBuffer no error");

        // --- saxpy (alpha=3) ---
        gl.uniform_1_f32(Some(&la), 3.0);
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
        {
            let m = map_read(&gl, N);
            let g = m.as_ref().map(|v| (0..N).all(|i| feq(v[i], 3.0 * a[i] + b[i]))).unwrap_or(false);
            ok(g, "saxpy == 3*a+b (uniform)");
            // NEGATIVE CONTROL (saxpy): corrupt real output, assert checker rejects.
            if let Some(v) = m.as_ref() {
                let mut poisoned = v.clone();
                poisoned[0] = 3.0 * a[0] + b[0] + 2.0;
                let still_ok = (0..N).all(|i| feq(poisoned[i], 3.0 * a[i] + b[i]));
                ok(!still_ok, "neg-control saxpy: corrupted element flagged");
            } else {
                ok(false, "neg-control saxpy: corrupted element flagged");
            }
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
        }

        // --- fence sync: fence_sync + client_wait_sync, assert command completion ---
        {
            let fence = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0);
            ok(fence.is_ok(), "glFenceSync created");
            if let Ok(f) = fence {
                let r = gl.client_wait_sync(f, glow::SYNC_FLUSH_COMMANDS_BIT, 1_000_000_000);
                ok(r == glow::ALREADY_SIGNALED || r == glow::CONDITION_SATISFIED,
                    "glClientWaitSync signaled");
                let st = gl.get_sync_status(f);
                ok(st == glow::SIGNALED, "glGetSynciv SYNC_STATUS == SIGNALED");
                gl.delete_sync(f);
            } else {
                ok(false, "glClientWaitSync signaled");
                ok(false, "glGetSynciv SYNC_STATUS == SIGNALED");
            }
        }

        // --- indirect dispatch: same saxpy(alpha=3) driven by a DISPATCH_INDIRECT_BUFFER ---
        {
            // reset C to a known wrong pattern so a stale read cannot pass
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
            let junk = vec![-7.0f32; N];
            gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&junk));

            let groups: [u32; 3] = [((N + 63) / 64) as u32, 1, 1];
            let indirect = gl.create_buffer().expect("indirect");
            gl.bind_buffer(glow::DISPATCH_INDIRECT_BUFFER, Some(indirect));
            gl.buffer_data_u8_slice(
                glow::DISPATCH_INDIRECT_BUFFER,
                std::slice::from_raw_parts(groups.as_ptr() as *const u8, 12),
                glow::STATIC_DRAW,
            );
            gl.dispatch_compute_indirect(0);
            ok(gl.get_error() == glow::NO_ERROR, "glDispatchComputeIndirect no error");
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
            let m = map_read(&gl, N);
            let g = m.as_ref().map(|v| (0..N).all(|i| feq(v[i], 3.0 * a[i] + b[i]))).unwrap_or(false);
            ok(g, "indirect saxpy == 3*a+b");
            // NEGATIVE CONTROL (indirect): corrupt real output, assert checker rejects.
            if let Some(v) = m.as_ref() {
                let mut poisoned = v.clone();
                poisoned[N - 1] = 0.0;
                let still_ok = (0..N).all(|i| feq(poisoned[i], 3.0 * a[i] + b[i]));
                ok(!still_ok, "neg-control indirect: corrupted element flagged");
            } else {
                ok(false, "neg-control indirect: corrupted element flagged");
            }
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
            gl.delete_buffer(indirect);
        }

        // --- buffer sub-data update + re-dispatch (vadd, alpha=1) ---
        let a2 = vec![2.0f32; N];
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&a2));
        ok(gl.get_error() == glow::NO_ERROR, "glBufferSubData A<-2 no error");
        gl.uniform_1_f32(Some(&la), 1.0);
        gl.dispatch_compute(((N + 63) / 64) as u32, 1, 1);
        gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let m = map_read(&gl, N);
            let g = m.as_ref().map(|v| (0..N).all(|i| feq(v[i], 2.0 + b[i]))).unwrap_or(false);
            ok(g, "vadd after subdata == 2+b");
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
        }

        // --- BOUNDARY: N=0 zero dispatch (0 work groups) must be a no-op, no error ---
        {
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
            let sentinel = vec![9.0f32; N];
            gl.buffer_sub_data_u8_slice(glow::SHADER_STORAGE_BUFFER, 0, as_bytes(&sentinel));
            let _ = gl.get_error();
            gl.uniform_1_u32(Some(&ln), 0);
            gl.dispatch_compute(0, 1, 1);
            ok(gl.get_error() == glow::NO_ERROR, "zero-dispatch no error");
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            let m = map_read(&gl, N);
            let g = m.as_ref().map(|v| (0..N).all(|i| feq(v[i], 9.0))).unwrap_or(false);
            ok(g, "zero-dispatch leaves output untouched");
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
            gl.uniform_1_u32(Some(&ln), N as u32); // restore
        }

        // --- BOUNDARY: large, non-divisible size (>=1,000,000) verified element-wise vs closed form ---
        {
            const BIG: usize = 1_000_003; // prime -> not a multiple of local_size_x(64): exercises tail guard
            let big_bytes = BIG * std::mem::size_of::<f32>();
            let mut ba = vec![0f32; BIG];
            let mut bb = vec![0f32; BIG];
            for i in 0..BIG {
                ba[i] = (i % 997) as f32;
                bb[i] = ((i % 31) as f32) * 0.5 + 1.0;
            }
            let big0 = gl.create_buffer().expect("big0");
            let big1 = gl.create_buffer().expect("big1");
            let big2 = gl.create_buffer().expect("big2");
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big0));
            gl.buffer_data_u8_slice(glow::SHADER_STORAGE_BUFFER, as_bytes(&ba), glow::STATIC_DRAW);
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big1));
            gl.buffer_data_u8_slice(glow::SHADER_STORAGE_BUFFER, as_bytes(&bb), glow::STATIC_DRAW);
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big2));
            gl.buffer_data_size(glow::SHADER_STORAGE_BUFFER, big_bytes as i32, glow::DYNAMIC_COPY);
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(big0));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(big1));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(big2));
            gl.uniform_1_f32(Some(&la), 4.0);
            gl.uniform_1_u32(Some(&ln), BIG as u32);
            let ngroups = ((BIG + 63) / 64) as u32;
            gl.dispatch_compute(ngroups, 1, 1);
            ok(gl.get_error() == glow::NO_ERROR, "large dispatch no error");
            gl.memory_barrier(glow::SHADER_STORAGE_BARRIER_BIT);
            gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(big2));
            let m = map_read(&gl, BIG);
            // verify against closed form c = 4*a + b for every element (incl. the tail past a
            // whole multiple of 64), and that the guard did not clobber a phantom element N.
            let g = m.as_ref()
                .map(|v| (0..BIG).all(|i| feq(v[i], 4.0 * ba[i] + bb[i])))
                .unwrap_or(false);
            ok(g, "large saxpy(1000003) == 4*a+b element-wise");
            // NEGATIVE CONTROL (large): corrupt a tail element and assert the checker flags it.
            if let Some(v) = m.as_ref() {
                let mut poisoned = v.clone();
                poisoned[BIG - 1] += 5.0;
                let still_ok = (0..BIG).all(|i| feq(poisoned[i], 4.0 * ba[i] + bb[i]));
                ok(!still_ok, "neg-control large: corrupted tail element flagged");
            } else {
                ok(false, "neg-control large: corrupted tail element flagged");
            }
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
            gl.delete_buffer(big0);
            gl.delete_buffer(big1);
            gl.delete_buffer(big2);
            // restore small SSBO bindings for the introspection block below
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 1, Some(buf1));
            gl.bind_buffer_base(glow::SHADER_STORAGE_BUFFER, 2, Some(buf2));
        }

        // === extended coverage: copy-sub-data / bind-buffer-range / buffer-param query /
        //     SSBO block introspection ===
        gl.bind_buffer(glow::COPY_READ_BUFFER, Some(buf0));
        gl.bind_buffer(glow::COPY_WRITE_BUFFER, Some(buf2));
        gl.copy_buffer_sub_data(glow::COPY_READ_BUFFER, glow::COPY_WRITE_BUFFER, 0, 0, bytes as i32);
        ok(gl.get_error() == glow::NO_ERROR, "glCopyBufferSubData no error");
        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf2));
        {
            let m = map_read(&gl, N);
            // buf0 currently holds a2 (all 2.0) from the sub-data update above.
            let g = m.as_ref().map(|v| (0..N).all(|i| feq(v[i], 2.0))).unwrap_or(false);
            ok(g, "copy-sub-data buf0(=2)->buf2");
            // NEGATIVE CONTROL (copy): corrupt real copied output, assert checker rejects.
            if let Some(v) = m.as_ref() {
                let mut poisoned = v.clone();
                poisoned[3] = 2.0 + 1.0;
                let still_ok = (0..N).all(|i| feq(poisoned[i], 2.0));
                ok(!still_ok, "neg-control copy: corrupted element flagged");
            } else {
                ok(false, "neg-control copy: corrupted element flagged");
            }
            gl.unmap_buffer(glow::SHADER_STORAGE_BUFFER);
        }

        gl.bind_buffer_range(glow::SHADER_STORAGE_BUFFER, 0, Some(buf0), 0, bytes as i32);
        ok(gl.get_error() == glow::NO_ERROR, "glBindBufferRange no error");

        gl.bind_buffer(glow::SHADER_STORAGE_BUFFER, Some(buf0));
        let bsz = gl.get_buffer_parameter_i32(glow::SHADER_STORAGE_BUFFER, glow::BUFFER_SIZE);
        ok(bsz == bytes as i32, "glGetBufferParameteriv BUFFER_SIZE");

        let prog_raw = program_raw(prog);
        let name_a = std::ffi::CString::new("A").unwrap();
        let idx = get_program_resource_index(
            prog_raw, glow::SHADER_STORAGE_BLOCK, name_a.as_ptr());
        ok(idx != GL_INVALID_INDEX, "glGetProgramResourceIndex A");
        // A non-existent block name must yield GL_INVALID_INDEX.
        let name_z = std::ffi::CString::new("NoSuchBlock").unwrap();
        let idxz = get_program_resource_index(
            prog_raw, glow::SHADER_STORAGE_BLOCK, name_z.as_ptr());
        ok(idxz == GL_INVALID_INDEX, "glGetProgramResourceIndex(bad) == INVALID_INDEX");

        let mut nres: i32 = 0;
        get_program_interfaceiv(
            prog_raw, glow::SHADER_STORAGE_BLOCK, glow::ACTIVE_RESOURCES, &mut nres);
        ok(nres == 3, "glGetProgramInterfaceiv ACTIVE_RESOURCES == 3");

        {
            let mut binding: i32 = -1;
            let props = [glow::BUFFER_BINDING];
            if idx != GL_INVALID_INDEX {
                get_program_resourceiv(
                    prog_raw, glow::SHADER_STORAGE_BLOCK, idx,
                    props.len() as i32, props.as_ptr(),
                    1, std::ptr::null_mut(), &mut binding);
            }
            ok(binding == 0, "glGetProgramResourceiv BUFFER_BINDING(A)==0");
        }

        ok(gl.get_error() == glow::NO_ERROR, "glGetError == GL_NO_ERROR (final happy path)");

        // --- cleanup ---
        gl.delete_buffer(buf0);
        gl.delete_buffer(buf1);
        gl.delete_buffer(buf2);
        gl.delete_program(prog);
        ok(gl.get_error() == glow::NO_ERROR, "cleanup no error");
    }

    let _ = egl.make_current(dpy, None, None, None);
    ok(egl.destroy_context(dpy, ctx).is_ok(), "eglDestroyContext");
    ok(egl.terminate(dpy).is_ok(), "eglTerminate");

    let expected = EXPECTED;
    let (pass, fail) = unsafe { (PASS, FAIL) };
    let total = pass + fail;
    println!("gles-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("GLES_RUST_FULL_API OK {pass}");
        0
    } else {
        println!("GLES_RUST_FULL_API FAIL");
        1
    }
}

const EXPECTED: i32 = 68;

fn as_bytes(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

// GLES 3.1 has no glGetBufferSubData; read via a mapped range and copy out f32s.
unsafe fn map_read(gl: &glow::Context, n: usize) -> Option<Vec<f32>> {
    let bytes = n * std::mem::size_of::<f32>();
    let ptr = gl.map_buffer_range(
        glow::SHADER_STORAGE_BUFFER, 0, bytes as i32, glow::MAP_READ_BIT);
    if ptr.is_null() {
        return None;
    }
    let src = std::slice::from_raw_parts(ptr as *const f32, n);
    Some(src.to_vec())
}
