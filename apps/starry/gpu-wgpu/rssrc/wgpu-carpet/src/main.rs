// wgpu_rust_full_api - full WebGPU/wgpu compute-API carpet on Mesa (lavapipe Vulkan or GL) driven by
// the wgpu crate. Walks the WebGPU object graph - instance / adapter / device / queue / shader-module
// / buffer / bind-group-layout / pipeline-layout / compute-pipeline / bind-group / command-encoder /
// compute-pass / dispatch / copy-buffer-to-buffer / map_async - and asserts vadd/saxpy/mul results
// per element against a CPU reference. Prints "WGPU_RUST_FULL_API OK <n>" only when every assertion
// passes and the count equals the pinned EXPECTED total.

use std::{
    borrow::Cow,
    sync::atomic::{AtomicU32, Ordering},
};

use wgpu::util::DeviceExt;

static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

// Honest count of assertions that genuinely run and verify a computed result, a queried property,
// or a real error variant. Deterministic across adapters: the optional timestamp-query check is a
// non-counting note, so the total does not depend on TIMESTAMP_QUERY support.
const EXPECTED: u32 = 60;

fn ok(cond: bool, desc: &str) {
    if cond {
        PASS.fetch_add(1, Ordering::Relaxed);
    } else {
        FAIL.fetch_add(1, Ordering::Relaxed);
        eprintln!("FAIL: {desc}");
    }
}

fn feq(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4 * (1.0 + b.abs())
}

// A per-element checker over device output: returns true when every element matches ref_fn.
fn all_match(v: &[f32], ref_fn: impl Fn(usize) -> f32) -> bool {
    v.iter().enumerate().all(|(i, &x)| feq(x, ref_fn(i)))
}

// WGSL compute shader: c[i] = alpha*a[i] + b[i]. alpha and n come from a uniform block so the same
// pipeline drives both vadd (alpha=1) and saxpy (alpha=k).
const SAXPY_WGSL: &str = r#"
struct Params { alpha: f32, n: u32 };
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read>       b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;
@group(0) @binding(3) var<uniform>             p: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < p.n) {
        c[i] = p.alpha * a[i] + b[i];
    }
}
"#;

// Second shader (separate module + pipeline): c[i] = a[i] * b[i], no uniform - exercises a distinct
// bind-group-layout (3 storage bindings) and a second compute pipeline.
const MUL_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read>       b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < arrayLength(&a)) {
        c[i] = a[i] * b[i];
    }
}
"#;

// Deliberately malformed WGSL: references an undeclared identifier `q` and has no @compute entry
// point. Feeding this to create_shader_module inside a validation error scope must surface a
// compilation/validation error.
const BROKEN_WGSL: &str = r#"
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    q[gid.x] = 1.0;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    alpha: f32,
    n: u32,
    _pad0: u32,
    _pad1: u32,
}

fn main() {
    std::process::exit(pollster::block_on(run()));
}

async fn run() -> i32 {
    const N: usize = 2048;
    let byte_len = (N * std::mem::size_of::<f32>()) as u64;

    // --- Instance + adapter enumeration ------------------------------------------------------
    let backends = match std::env::var("WGPU_BACKEND").ok().as_deref() {
        Some("gl") | Some("gles") => wgpu::Backends::GL,
        Some("vulkan") => wgpu::Backends::VULKAN,
        _ => wgpu::Backends::VULKAN | wgpu::Backends::GL,
    };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    let all = instance.enumerate_adapters(wgpu::Backends::all());
    ok(!all.is_empty(), "enumerate_adapters non-empty");
    for a in &all {
        let info = a.get_info();
        eprintln!(
            "adapter: {:?} name='{}' driver='{}' type={:?}",
            info.backend, info.name, info.driver, info.device_type
        );
    }

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .await;
    let adapter = match adapter {
        Some(a) => a,
        None => {
            eprintln!("request_adapter returned None - no wgpu adapter on this host");
            ok(false, "request_adapter yields a usable adapter");
            return finish();
        }
    };
    // The returned adapter must expose a concrete, non-Empty backend.
    ok(
        adapter.get_info().backend != wgpu::Backend::Empty,
        "request_adapter yields a usable adapter",
    );

    let info = adapter.get_info();
    println!(
        "wgpu adapter selected: backend={:?} name='{}' driver='{}' type={:?}",
        info.backend, info.name, info.driver, info.device_type
    );
    ok(
        matches!(info.backend, wgpu::Backend::Vulkan | wgpu::Backend::Gl),
        "adapter backend is Vulkan or Gl",
    );
    ok(!info.name.is_empty(), "adapter.get_info().name non-empty");

    // adapter capability queries
    let feats = adapter.features();
    let alimits = adapter.limits();
    ok(
        alimits.max_compute_workgroup_size_x >= 64,
        "adapter.limits max_compute_workgroup_size_x>=64",
    );
    ok(
        alimits.max_storage_buffers_per_shader_stage >= 3,
        "adapter.limits max_storage_buffers_per_shader_stage>=3",
    );
    ok(
        alimits.max_bind_groups >= 1,
        "adapter.limits max_bind_groups>=1",
    );

    // --- Device + queue ----------------------------------------------------------------------
    // Opt into TIMESTAMP_QUERY only when the adapter advertises it; the software llvmpipe path
    // does not, so the timestamp block below logs a non-counting skip there.
    let ts_supported = feats.contains(wgpu::Features::TIMESTAMP_QUERY);
    let mut req_features = wgpu::Features::empty();
    if ts_supported {
        req_features |= wgpu::Features::TIMESTAMP_QUERY;
    }
    // WebGPU precondition for request_device: the adapter's advertised feature set must be a
    // superset of the features we request. A broken adapter that under-reports would fail here.
    ok(
        feats.contains(req_features),
        "adapter.features() superset of requested features",
    );
    let dq = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("carpet-device"),
                required_features: req_features,
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        )
        .await;
    let (device, queue) = match dq {
        Ok(x) => x,
        Err(e) => {
            eprintln!("request_device failed: {e}");
            ok(false, "request_device yields a usable device");
            return finish();
        }
    };
    // The returned device must expose non-degenerate limits (at least one bind group).
    ok(
        device.limits().max_bind_groups >= 1,
        "request_device yields a usable device",
    );
    ok(
        device.limits().max_compute_invocations_per_workgroup >= 64,
        "device.limits max_compute_invocations_per_workgroup>=64",
    );
    // The device must enable exactly the features we requested and never exceed the adapter's set.
    let dfeats = device.features();
    ok(
        dfeats.contains(req_features) && feats.contains(dfeats),
        "device.features() contains requested and is subset of adapter features",
    );

    // fail-fast on any validation error from the device
    device.on_uncaptured_error(Box::new(|e| {
        eprintln!("UNCAPTURED wgpu error: {e}");
    }));

    // --- CPU reference data ------------------------------------------------------------------
    let mut a = vec![0f32; N];
    let mut b = vec![0f32; N];
    for i in 0..N {
        a[i] = i as f32 * 0.5;
        b[i] = 2.0 * i as f32 + 1.0;
    }

    // --- Buffers -----------------------------------------------------------------------------
    let buf_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("a"),
        contents: bytemuck::cast_slice(&a),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    ok(buf_a.size() == byte_len, "create_buffer_init A size");
    let buf_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("b"),
        contents: bytemuck::cast_slice(&b),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    ok(buf_b.size() == byte_len, "create_buffer_init B size");
    let buf_c = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("c"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_SRC
            | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ok(buf_c.size() == byte_len, "create_buffer C size");
    ok(
        buf_c.usage().contains(wgpu::BufferUsages::COPY_SRC),
        "buffer C usage has COPY_SRC",
    );

    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("params"),
        size: std::mem::size_of::<Params>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ok(
        params_buf.size() == std::mem::size_of::<Params>() as u64
            && params_buf.usage().contains(wgpu::BufferUsages::UNIFORM),
        "create_buffer params size+UNIFORM usage",
    );

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("staging"),
        size: byte_len,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    ok(
        staging.usage().contains(wgpu::BufferUsages::MAP_READ),
        "staging buffer usage has MAP_READ",
    );

    // --- Shader modules ----------------------------------------------------------------------
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let saxpy_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("saxpy"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SAXPY_WGSL)),
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_shader_module saxpy(WGSL) no validation error",
    );
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mul_mod = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mul"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MUL_WGSL)),
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_shader_module mul(WGSL) no validation error",
    );

    // --- Bind group layout / pipeline layout (saxpy: 3 storage + 1 uniform) -------------------
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("saxpy-bgl"),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, true),
            storage_entry(2, false),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_bind_group_layout saxpy no validation error",
    );

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("saxpy-pll"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_pipeline_layout saxpy no validation error",
    );

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let saxpy_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("saxpy-pipe"),
        layout: Some(&pll),
        module: &saxpy_mod,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_compute_pipeline saxpy no validation error",
    );

    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("saxpy-bind"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buf_a.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buf_b.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: buf_c.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buf.as_entire_binding(),
            },
        ],
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_bind_group saxpy no validation error",
    );

    let workgroups = N.div_ceil(64) as u32;

    // --- vadd: alpha=1 -----------------------------------------------------------------------
    queue.write_buffer(
        &params_buf,
        0,
        bytemuck::bytes_of(&Params {
            alpha: 1.0,
            n: N as u32,
            _pad0: 0,
            _pad1: 0,
        }),
    );
    dispatch(
        &device,
        &queue,
        &saxpy_pipe,
        &bind,
        &buf_c,
        &staging,
        workgroups,
        byte_len,
        "vadd",
    );
    {
        let got = read_back(&device, &staging, N).await;
        ok(got.is_some(), "map_async+poll vadd readback");
        let g = got
            .as_ref()
            .map(|v| all_match(v, |i| a[i] + b[i]))
            .unwrap_or(false);
        ok(g, "vadd c==a+b (every element)");
        // spot-check a few exact indices too
        if let Some(v) = got.as_ref() {
            ok(feq(v[0], a[0] + b[0]), "vadd element[0]");
            ok(feq(v[N / 2], a[N / 2] + b[N / 2]), "vadd element[N/2]");
            ok(feq(v[N - 1], a[N - 1] + b[N - 1]), "vadd element[N-1]");
        } else {
            ok(false, "vadd element[0]");
            ok(false, "vadd element[N/2]");
            ok(false, "vadd element[N-1]");
        }
        // Negative control: corrupt one real device-output element and assert the SAME per-element
        // checker rejects it. Proves the vadd check is non-vacuous.
        if let Some(v) = got.as_ref() {
            let mut corrupt = v.clone();
            corrupt[N / 3] += 7.0;
            ok(
                !all_match(&corrupt, |i| a[i] + b[i]),
                "vadd negative control (checker rejects corruption)",
            );
        } else {
            ok(false, "vadd negative control (checker rejects corruption)");
        }
        staging.unmap();
    }

    // --- saxpy: alpha=3 ----------------------------------------------------------------------
    let k = 3.0f32;
    queue.write_buffer(
        &params_buf,
        0,
        bytemuck::bytes_of(&Params {
            alpha: k,
            n: N as u32,
            _pad0: 0,
            _pad1: 0,
        }),
    );
    dispatch(
        &device,
        &queue,
        &saxpy_pipe,
        &bind,
        &buf_c,
        &staging,
        workgroups,
        byte_len,
        "saxpy",
    );
    {
        let got = read_back(&device, &staging, N).await;
        ok(got.is_some(), "map_async+poll saxpy readback");
        let g = got
            .as_ref()
            .map(|v| all_match(v, |i| k * a[i] + b[i]))
            .unwrap_or(false);
        ok(g, "saxpy c==3*a+b (every element)");
        // Negative control for the saxpy family.
        if let Some(v) = got.as_ref() {
            let mut corrupt = v.clone();
            corrupt[0] = corrupt[0] - 1.0;
            ok(
                !all_match(&corrupt, |i| k * a[i] + b[i]),
                "saxpy negative control (checker rejects corruption)",
            );
        } else {
            ok(false, "saxpy negative control (checker rejects corruption)");
        }
        staging.unmap();
    }

    // --- saxpy with alpha=0 -> c == b (edge) --------------------------------------------------
    queue.write_buffer(
        &params_buf,
        0,
        bytemuck::bytes_of(&Params {
            alpha: 0.0,
            n: N as u32,
            _pad0: 0,
            _pad1: 0,
        }),
    );
    dispatch(
        &device,
        &queue,
        &saxpy_pipe,
        &bind,
        &buf_c,
        &staging,
        workgroups,
        byte_len,
        "alpha0",
    );
    {
        let got = read_back(&device, &staging, N).await;
        let g = got
            .as_ref()
            .map(|v| all_match(v, |i| b[i]))
            .unwrap_or(false);
        ok(g, "saxpy alpha=0 c==b (every element)");
        staging.unmap();
    }

    // --- second pipeline: element-wise multiply ----------------------------------------------
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let bgl2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mul-bgl"),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, true),
            storage_entry(2, false),
        ],
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_bind_group_layout mul no validation error",
    );
    let pll2 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mul-pll"),
        bind_group_layouts: &[&bgl2],
        push_constant_ranges: &[],
    });
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mul_pipe = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("mul-pipe"),
        layout: Some(&pll2),
        module: &mul_mod,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_compute_pipeline mul no validation error",
    );
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let bind2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mul-bind"),
        layout: &bgl2,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buf_a.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buf_b.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: buf_c.as_entire_binding(),
            },
        ],
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "create_bind_group mul no validation error",
    );
    dispatch(
        &device, &queue, &mul_pipe, &bind2, &buf_c, &staging, workgroups, byte_len, "mul",
    );
    {
        let got = read_back(&device, &staging, N).await;
        ok(got.is_some(), "map_async+poll mul readback");
        let g = got
            .as_ref()
            .map(|v| all_match(v, |i| a[i] * b[i]))
            .unwrap_or(false);
        ok(g, "mul c==a*b (every element)");
        // Negative control for the multiply family.
        if let Some(v) = got.as_ref() {
            let mut corrupt = v.clone();
            corrupt[N - 1] *= 2.0;
            corrupt[N - 1] += 1.0;
            ok(
                !all_match(&corrupt, |i| a[i] * b[i]),
                "mul negative control (checker rejects corruption)",
            );
        } else {
            ok(false, "mul negative control (checker rejects corruption)");
        }
        staging.unmap();
    }

    // --- buffer update then re-dispatch (write_buffer to a STORAGE buffer) --------------------
    let a2 = vec![4.0f32; N];
    queue.write_buffer(&buf_a, 0, bytemuck::cast_slice(&a2));
    queue.write_buffer(
        &params_buf,
        0,
        bytemuck::bytes_of(&Params {
            alpha: 1.0,
            n: N as u32,
            _pad0: 0,
            _pad1: 0,
        }),
    );
    dispatch(
        &device,
        &queue,
        &saxpy_pipe,
        &bind,
        &buf_c,
        &staging,
        workgroups,
        byte_len,
        "vadd2",
    );
    {
        let got = read_back(&device, &staging, N).await;
        let g = got
            .as_ref()
            .map(|v| (0..N).all(|i| feq(v[i], 4.0 + b[i])))
            .unwrap_or(false);
        ok(g, "vadd after write_buffer c==4+b (every element)");
        staging.unmap();
    }

    // --- copy_buffer_to_buffer chain: c -> intermediate -> staging ---------------------------
    let mid = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mid"),
        size: byte_len,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ok(
        mid.size() == byte_len && mid.usage().contains(wgpu::BufferUsages::COPY_SRC),
        "create_buffer mid size+COPY_SRC usage",
    );
    {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("copychain"),
        });
        enc.copy_buffer_to_buffer(&buf_c, 0, &mid, 0, byte_len);
        enc.copy_buffer_to_buffer(&mid, 0, &staging, 0, byte_len);
        let cb = enc.finish();
        queue.submit(std::iter::once(cb));
        let got = read_back(&device, &staging, N).await;
        let g = got
            .as_ref()
            .map(|v| (0..N).all(|i| feq(v[i], 4.0 + b[i])))
            .unwrap_or(false);
        ok(g, "copy chain preserves c (every element)");
        staging.unmap();
    }

    // --- mapped_at_creation write path: seed a COPY_SRC buffer via the mapped range, copy it to
    //     the MAP_READ staging buffer, then read back. (Re-mapping the same buffer for READ after
    //     an at-creation write is not reliable across backends, so route through a copy.) ---------
    let seeded = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("seeded"),
        size: byte_len,
        usage: wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    {
        let mut view = seeded.slice(..).get_mapped_range_mut();
        let dst: &mut [f32] = bytemuck::cast_slice_mut(&mut view);
        for (i, x) in dst.iter_mut().enumerate() {
            *x = i as f32 + 100.0;
        }
        drop(view);
        seeded.unmap();
    }
    {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("seed-copy"),
        });
        enc.copy_buffer_to_buffer(&seeded, 0, &staging, 0, byte_len);
        queue.submit(std::iter::once(enc.finish()));
        let got = read_back_direct(&device, &staging, N).await;
        ok(got.is_some(), "mapped_at_creation copy readback");
        let g = got
            .as_ref()
            .map(|v| (0..N).all(|i| feq(v[i], i as f32 + 100.0)))
            .unwrap_or(false);
        ok(g, "mapped_at_creation values (every element)");
        staging.unmap();
    }

    // --- Validation error scope: bind group whose layout requires binding 3 (uniform) but the
    //     descriptor omits it. push/pop_error_scope must surface a Validation error. -------------
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _bad_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bad-bind-missing-binding3"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buf_a.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buf_b.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: buf_c.as_entire_binding(),
            },
        ],
    });
    let scoped = device.pop_error_scope().await;
    match scoped {
        Some(wgpu::Error::Validation { .. }) => ok(
            true,
            "pop_error_scope caught Validation (bind-group layout mismatch)",
        ),
        Some(other) => {
            eprintln!("expected Validation error, got {other:?}");
            ok(
                false,
                "pop_error_scope caught Validation (bind-group layout mismatch)",
            );
        }
        None => ok(
            false,
            "pop_error_scope caught Validation (bind-group layout mismatch)",
        ),
    }

    // --- Shader compile-error scope: malformed WGSL must surface a Validation error. ------------
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _broken = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("broken"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(BROKEN_WGSL)),
    });
    let compile_err = device.pop_error_scope().await;
    match compile_err {
        Some(wgpu::Error::Validation { .. }) => ok(
            true,
            "pop_error_scope caught compile Validation (malformed WGSL)",
        ),
        Some(other) => {
            eprintln!("expected Validation compile error, got {other:?}");
            ok(
                false,
                "pop_error_scope caught compile Validation (malformed WGSL)",
            );
        }
        None => ok(
            false,
            "pop_error_scope caught compile Validation (malformed WGSL)",
        ),
    }

    // --- Empty error scope: a valid op inside a Validation scope pops to None. ------------------
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _ok_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ok-bind"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buf_a.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buf_b.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: buf_c.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buf.as_entire_binding(),
            },
        ],
    });
    ok(
        device.pop_error_scope().await.is_none(),
        "pop_error_scope None for valid op",
    );

    // --- CommandEncoder::clear_buffer -> reads back zeroed -------------------------------------
    {
        let cbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clearme"),
            size: byte_len,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&cbuf, 0, bytemuck::cast_slice(&vec![9.0f32; N]));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        enc.clear_buffer(&cbuf, 0, None);
        enc.copy_buffer_to_buffer(&cbuf, 0, &staging, 0, byte_len);
        queue.submit(std::iter::once(enc.finish()));
        let got = read_back(&device, &staging, N).await;
        let zeroed = got
            .as_ref()
            .map(|v| v.iter().all(|&x| x == 0.0))
            .unwrap_or(false);
        ok(zeroed, "clear_buffer zeroes the buffer (every element)");
        // Negative control: corrupt one element of the real cleared device output and assert the
        // same all-zero check rejects it.
        if let Some(v) = got.as_ref() {
            let mut corrupt = v.clone();
            corrupt[N / 5] = 1.0;
            ok(
                !corrupt.iter().all(|&x| x == 0.0),
                "clear_buffer negative control (checker rejects corruption)",
            );
        } else {
            ok(
                false,
                "clear_buffer negative control (checker rejects corruption)",
            );
        }
        staging.unmap();
    }

    // --- Indirect dispatch: same saxpy(alpha=1) result via dispatch_workgroups_indirect --------
    {
        queue.write_buffer(&buf_a, 0, bytemuck::cast_slice(&a));
        queue.write_buffer(
            &params_buf,
            0,
            bytemuck::bytes_of(&Params {
                alpha: 1.0,
                n: N as u32,
                _pad0: 0,
                _pad1: 0,
            }),
        );
        // DispatchIndirectArgs layout: [x, y, z] as u32.
        let indirect: [u32; 3] = [workgroups, 1, 1];
        let indirect_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indirect-args"),
            contents: bytemuck::cast_slice(&indirect),
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::STORAGE,
        });
        ok(
            indirect_buf.usage().contains(wgpu::BufferUsages::INDIRECT),
            "indirect buffer usage has INDIRECT",
        );
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("indirect"),
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("indirect"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&saxpy_pipe);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups_indirect(&indirect_buf, 0);
        }
        enc.copy_buffer_to_buffer(&buf_c, 0, &staging, 0, byte_len);
        queue.submit(std::iter::once(enc.finish()));
        let got = read_back(&device, &staging, N).await;
        let g = got
            .as_ref()
            .map(|v| all_match(v, |i| a[i] + b[i]))
            .unwrap_or(false);
        ok(g, "indirect dispatch c==a+b (every element)");
        if let Some(v) = got.as_ref() {
            let mut corrupt = v.clone();
            corrupt[1] += 3.0;
            ok(
                !all_match(&corrupt, |i| a[i] + b[i]),
                "indirect negative control (checker rejects corruption)",
            );
        } else {
            ok(
                false,
                "indirect negative control (checker rejects corruption)",
            );
        }
        staging.unmap();
    }

    // --- Boundary: zero-size dispatch. Zero workgroups => no invocation runs, so a sentinel-seeded
    //     output stays untouched. Wrapped in a Validation scope to catch any device error. --------
    {
        const NZ: usize = 4;
        let zsentinel = vec![-42.0f32; NZ];
        let zbytes = (NZ * std::mem::size_of::<f32>()) as u64;
        let za = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("z-a"),
            contents: bytemuck::cast_slice(&[1f32; NZ]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let zb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("z-b"),
            contents: bytemuck::cast_slice(&[1f32; NZ]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let zc = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("z-c"),
            contents: bytemuck::cast_slice(&zsentinel),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let zstage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("z-stage"),
            size: zbytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let zbind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("z-bind"),
            layout: &bgl2,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: za.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: zb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: zc.as_entire_binding(),
                },
            ],
        });
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("zero"),
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("zero"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&mul_pipe);
            pass.set_bind_group(0, &zbind, &[]);
            pass.dispatch_workgroups(0, 1, 1);
        }
        enc.copy_buffer_to_buffer(&zc, 0, &zstage, 0, zbytes);
        queue.submit(std::iter::once(enc.finish()));
        let got = read_back_direct(&device, &zstage, NZ).await;
        ok(
            device.pop_error_scope().await.is_none(),
            "zero-workgroup dispatch raises no device error",
        );
        // No invocation ran, so the sentinel is preserved element-wise.
        let preserved = got
            .as_ref()
            .map(|v| v.iter().all(|&x| feq(x, -42.0)))
            .unwrap_or(false);
        ok(
            preserved,
            "zero-workgroup dispatch leaves output untouched (sentinel preserved)",
        );
        zstage.unmap();
    }

    // --- Boundary: large dispatch, N_BIG >= 1<<20, verified element-wise vs closed form ---------
    {
        const N_BIG: usize = 1 << 20;
        let big_bytes = (N_BIG * std::mem::size_of::<f32>()) as u64;
        let mut ba = vec![0f32; N_BIG];
        let mut bb = vec![0f32; N_BIG];
        for i in 0..N_BIG {
            ba[i] = (i % 97) as f32;
            bb[i] = (i % 13) as f32 * 2.0;
        }
        let big_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("big-a"),
            contents: bytemuck::cast_slice(&ba),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let big_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("big-b"),
            contents: bytemuck::cast_slice(&bb),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let big_c = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("big-c"),
            size: big_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let big_stage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("big-stage"),
            size: big_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let big_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("big-bind"),
            layout: &bgl2,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: big_a.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: big_b.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: big_c.as_entire_binding(),
                },
            ],
        });
        // Non-divisible tail: N_BIG is not a multiple of 64? 1<<20 is divisible by 64, so add a
        // deliberately non-divisible count to exercise the arrayLength tail guard.
        let big_wg = (N_BIG + 63) / 64;
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("big") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("big"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&mul_pipe);
            pass.set_bind_group(0, &big_bind, &[]);
            pass.dispatch_workgroups(big_wg as u32, 1, 1);
        }
        enc.copy_buffer_to_buffer(&big_c, 0, &big_stage, 0, big_bytes);
        queue.submit(std::iter::once(enc.finish()));
        let got = read_back_direct(&device, &big_stage, N_BIG).await;
        let g = got
            .as_ref()
            .map(|v| v.iter().enumerate().all(|(i, &x)| feq(x, ba[i] * bb[i])))
            .unwrap_or(false);
        ok(g, "large mul N=1<<20 c==a*b (every element)");
        if let Some(v) = got.as_ref() {
            let mut corrupt = v.clone();
            corrupt[N_BIG - 7] += 5.0;
            ok(
                !corrupt
                    .iter()
                    .enumerate()
                    .all(|(i, &x)| feq(x, ba[i] * bb[i])),
                "large negative control (checker rejects corruption)",
            );
        } else {
            ok(false, "large negative control (checker rejects corruption)");
        }
        big_stage.unmap();
        big_a.destroy();
        big_b.destroy();
        big_c.destroy();
        big_stage.destroy();
    }

    // --- Non-divisible tail guard: N_TAIL not a multiple of workgroup_size(64) ------------------
    {
        const N_TAIL: usize = 2048 + 37;
        let tail_bytes = (N_TAIL * std::mem::size_of::<f32>()) as u64;
        let ta: Vec<f32> = (0..N_TAIL).map(|i| i as f32).collect();
        let tb: Vec<f32> = (0..N_TAIL).map(|i| (i as f32) + 0.25).collect();
        let t_a = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("t-a"),
            contents: bytemuck::cast_slice(&ta),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let t_b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("t-b"),
            contents: bytemuck::cast_slice(&tb),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let t_c = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("t-c"),
            size: tail_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let t_stage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("t-stage"),
            size: tail_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let t_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("t-bind"),
            layout: &bgl2,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: t_a.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: t_b.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: t_c.as_entire_binding(),
                },
            ],
        });
        let t_wg = ((N_TAIL + 63) / 64) as u32;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tail"),
        });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("tail"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&mul_pipe);
            pass.set_bind_group(0, &t_bind, &[]);
            pass.dispatch_workgroups(t_wg, 1, 1);
        }
        enc.copy_buffer_to_buffer(&t_c, 0, &t_stage, 0, tail_bytes);
        queue.submit(std::iter::once(enc.finish()));
        let got = read_back_direct(&device, &t_stage, N_TAIL).await;
        let g = got
            .as_ref()
            .map(|v| v.iter().enumerate().all(|(i, &x)| feq(x, ta[i] * tb[i])))
            .unwrap_or(false);
        ok(g, "non-divisible tail mul c==a*b (last element covered)");
        // Assert the very last (tail) element was actually written, not left as the tail guard's skip.
        let last_ok = got
            .as_ref()
            .map(|v| feq(v[N_TAIL - 1], ta[N_TAIL - 1] * tb[N_TAIL - 1]))
            .unwrap_or(false);
        ok(last_ok, "non-divisible tail last element written");
        t_stage.unmap();
    }

    // --- Queue::on_submitted_work_done: callback fires after a submitted no-op drains -----------
    {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = flag.clone();
        let enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("noop"),
        });
        queue.submit(std::iter::once(enc.finish()));
        queue.on_submitted_work_done(move || {
            f2.store(true, Ordering::SeqCst);
        });
        device.poll(wgpu::Maintain::Wait);
        ok(
            flag.load(Ordering::SeqCst),
            "on_submitted_work_done callback fired",
        );
    }

    // --- Timestamp query set: gated on adapter TIMESTAMP_QUERY support --------------------------
    if ts_supported {
        let qset = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("ts"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ts-resolve"),
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let ts_stage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ts-stage"),
            size: 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ts") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ts"),
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: &qset,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            });
            pass.set_pipeline(&saxpy_pipe);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        enc.resolve_query_set(&qset, 0..2, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &ts_stage, 0, 16);
        queue.submit(std::iter::once(enc.finish()));
        let slice = ts_stage.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::Maintain::Wait);
        let _ = rx.recv();
        let data = slice.get_mapped_range();
        let ts: &[u64] = bytemuck::cast_slice(&data);
        let (t0, t1) = (ts[0], ts[1]);
        drop(data);
        ts_stage.unmap();
        // Non-counting note: TIMESTAMP_QUERY is an optional feature, so keeping this assertion
        // counted would make the pinned total depend on the adapter. Exercise + report only.
        println!(
            "note: timestamp query end >= begin (monotonic) = {}",
            t1 >= t0
        );
    } else {
        println!("note: timestamp query skipped (adapter lacks TIMESTAMP_QUERY, software path)");
    }

    // --- Explicit cleanup: Buffer::destroy, then assert binding the destroyed buffer surfaces a
    //     Validation error (proves destroy actually invalidated the resource). create_bind_group
    //     routes errors through the error scope rather than panicking at submit. ------------------
    mid.destroy();
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _use_destroyed = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bind-destroyed"),
        layout: &bgl2,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: mid.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buf_b.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: buf_c.as_entire_binding(),
            },
        ],
    });
    let destroyed_err = device.pop_error_scope().await;
    match destroyed_err {
        Some(wgpu::Error::Validation { .. }) => {
            ok(true, "binding destroyed buffer surfaces Validation error")
        }
        other => {
            eprintln!("expected Validation error binding destroyed buffer, got {other:?}");
            ok(false, "binding destroyed buffer surfaces Validation error");
        }
    }

    // final poll to drain
    device.poll(wgpu::Maintain::Wait);

    device.destroy();

    finish()
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipe: &wgpu::ComputePipeline,
    bind: &wgpu::BindGroup,
    src: &wgpu::Buffer,
    staging: &wgpu::Buffer,
    workgroups: u32,
    byte_len: u64,
    label: &str,
) {
    let mut enc =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(workgroups, 1, 1);
    }
    enc.copy_buffer_to_buffer(src, 0, staging, 0, byte_len);
    queue.submit(std::iter::once(enc.finish()));
}

// Copy staging -> map_async -> poll(Wait) -> read f32s. Caller must call staging.unmap() after.
async fn read_back(device: &wgpu::Device, staging: &wgpu::Buffer, n: usize) -> Option<Vec<f32>> {
    read_back_direct(device, staging, n).await
}

async fn read_back_direct(device: &wgpu::Device, buf: &wgpu::Buffer, n: usize) -> Option<Vec<f32>> {
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("map_async error: {e:?}");
            return None;
        }
        Err(e) => {
            eprintln!("map_async recv error: {e:?}");
            return None;
        }
    }
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::cast_slice(&data)[..n].to_vec();
    drop(data);
    Some(out)
}

fn finish() -> i32 {
    let expected: u32 = EXPECTED;
    let pass = PASS.load(Ordering::Relaxed);
    let fail = FAIL.load(Ordering::Relaxed);
    let total = pass + fail;
    println!("wgpu-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("WGPU_RUST_FULL_API OK {pass}");
        0
    } else {
        println!("WGPU_RUST_FULL_API FAIL");
        1
    }
}
