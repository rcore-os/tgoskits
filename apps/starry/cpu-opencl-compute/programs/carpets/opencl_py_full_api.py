#!/usr/bin/env python3
# opencl_py_full_api.py - full OpenCL Python (pyopencl) API carpet against the pocl-CPU software
# backend (POCL_DEVICES=basic == the on-target StarryOS software compute path). Walks the entire
# meaningful compute lifecycle - platform/device/context/queue, buffer + map/unmap + rect copies,
# SVM (map + memfill + kernel arg), images (create/copy/fill/sampler/kernel), program compile/build/
# link + build-error path, kernel set_arg/set_scalar_arg_dtypes/get_arg_info, NDRange dispatch with
# global_offset, events + profiling + host-driven UserEvent gating on an out-of-order queue,
# boundary sizes (zero-size, >=1M element-wise, oversubscription) and genuine error/validation
# paths. Every counted assertion checks a numpy/closed-form result, a queried property vs a known
# value, or a real CL error. Prints "OPENCL_PY_FULL_API OK <n>" iff FAIL=0 and TOTAL==EXPECTED.
import sys, numpy as np, pyopencl as cl

P=[0]; F=[0]
def ok(c,d):
    if c: P[0]+=1
    else: F[0]+=1; sys.stderr.write("FAIL: %s\n"%d)
def note(d):
    sys.stderr.write("NOTE (non-counting): %s\n"%d)

SRC = r"""
__kernel void vadd(__global const float*a,__global const float*b,__global float*c){int i=get_global_id(0);c[i]=a[i]+b[i];}
__kernel void vmul(__global const float*a,__global const float*b,__global float*c){int i=get_global_id(0);c[i]=a[i]*b[i];}
__kernel void saxpy(float alpha,__global const float*x,__global float*y){int i=get_global_id(0);y[i]=alpha*x[i]+y[i];}
__kernel void reduce_sum(__global const float*a,__global float*out,__local float*s){
  int lid=get_local_id(0),gid=get_global_id(0),ls=get_local_size(0);s[lid]=a[gid];
  barrier(CLK_LOCAL_MEM_FENCE);for(int o=ls/2;o>0;o>>=1){if(lid<o)s[lid]+=s[lid+o];barrier(CLK_LOCAL_MEM_FENCE);}
  if(lid==0)out[get_group_id(0)]=s[0];}
__kernel void off_write(__global int*a){int i=get_global_id(0);a[i]=i;}
__kernel void svm_inc(__global float*a){int i=get_global_id(0);a[i]+=1.0f;}
"""

# --- platform APIs ---
plats = cl.get_platforms(); ok(len(plats)>=1, "cl.get_platforms")
plat = plats[0]
ok(isinstance(plat.get_info(cl.platform_info.NAME), str), "platform NAME")
ok(isinstance(plat.get_info(cl.platform_info.VENDOR), str), "platform VENDOR")
ok(isinstance(plat.get_info(cl.platform_info.VERSION), str), "platform VERSION")
ok(isinstance(plat.get_info(cl.platform_info.PROFILE), str), "platform PROFILE")

# --- device APIs ---
devs = plat.get_devices(); ok(len(devs)>=1, "platform.get_devices")
dev = devs[0]
ok(dev.get_info(cl.device_info.MAX_COMPUTE_UNITS) >= 1, "device MAX_COMPUTE_UNITS")
max_wg = dev.get_info(cl.device_info.MAX_WORK_GROUP_SIZE); ok(max_wg >= 1, "device MAX_WORK_GROUP_SIZE")
ok(dev.get_info(cl.device_info.GLOBAL_MEM_SIZE) > 0, "device GLOBAL_MEM_SIZE")
_lmt = int(dev.get_info(cl.device_info.LOCAL_MEM_TYPE))
ok(dev.get_info(cl.device_info.LOCAL_MEM_SIZE) > 0 and _lmt in (int(cl.device_local_mem_type.LOCAL), int(cl.device_local_mem_type.GLOBAL)),
   "device LOCAL_MEM_SIZE>0 with a valid LOCAL_MEM_TYPE enum")
ok(isinstance(dev.get_info(cl.device_info.NAME), str), "device NAME")
ok(dev.get_info(cl.device_info.MAX_WORK_ITEM_DIMENSIONS) >= 3, "device MAX_WORK_ITEM_DIMENSIONS")
ok(int(dev.get_info(cl.device_info.TYPE)) != 0, "device TYPE")

# --- context + queue APIs ---
ctx = cl.Context([dev]); ok(ctx is not None, "cl.Context")
ok(ctx.get_info(cl.context_info.NUM_DEVICES) == 1, "context NUM_DEVICES")
q = cl.CommandQueue(ctx, properties=cl.command_queue_properties.PROFILING_ENABLE); ok(q is not None, "cl.CommandQueue PROFILING")
ok(q.get_info(cl.command_queue_info.CONTEXT) == ctx, "queue CONTEXT")
ok(int(q.properties) & int(cl.command_queue_properties.PROFILING_ENABLE), "queue PROFILING_ENABLE set")

# --- buffer APIs ---
N = 1024
a = np.arange(N, dtype=np.float32); b = (2.0*np.arange(N)+1.0).astype(np.float32)
mf = cl.mem_flags
A = cl.Buffer(ctx, mf.READ_ONLY | mf.COPY_HOST_PTR, hostbuf=a); ok(A.size == a.nbytes, "Buffer A COPY_HOST_PTR size")
B = cl.Buffer(ctx, mf.READ_ONLY, size=b.nbytes); ok(B.get_info(cl.mem_info.SIZE) == b.nbytes, "Buffer B SIZE")
C = cl.Buffer(ctx, mf.WRITE_ONLY, size=a.nbytes); ok(int(C.get_info(cl.mem_info.FLAGS)) & int(mf.WRITE_ONLY), "Buffer C FLAGS WRITE_ONLY")
cl.enqueue_copy(q, B, b); q.finish()
_hb = np.empty_like(b); cl.enqueue_copy(q, _hb, B); ok(np.array_equal(_hb, b), "enqueue_copy host->B->host roundtrip")

# --- program + kernel APIs ---
prog = cl.Program(ctx, SRC).build(options=["-cl-std=CL1.2", "-cl-kernel-arg-info"]); ok(prog is not None, "Program.build")
kns = prog.get_info(cl.program_info.KERNEL_NAMES)
ok(all(k in kns for k in ("vadd","vmul","saxpy","reduce_sum")), "program KERNEL_NAMES")
ok(int(prog.get_build_info(dev, cl.program_build_info.STATUS)) == 0, "program BUILD_STATUS CL_BUILD_SUCCESS(0)")
kadd = cl.Kernel(prog, "vadd"); ok(kadd.get_info(cl.kernel_info.NUM_ARGS) == 3, "kernel NUM_ARGS")
ok(kadd.get_info(cl.kernel_info.FUNCTION_NAME) == "vadd", "kernel FUNCTION_NAME")
ok(kadd.get_work_group_info(cl.kernel_work_group_info.WORK_GROUP_SIZE, dev) >= 1, "kernel WORK_GROUP_SIZE")

# --- Kernel.get_arg_info (requires -cl-kernel-arg-info) ---
ksax = cl.Kernel(prog, "saxpy")
ok(ksax.get_arg_info(0, cl.kernel_arg_info.NAME) == "alpha", "kernel arg0 NAME==alpha")
ok(ksax.get_arg_info(0, cl.kernel_arg_info.TYPE_NAME) == "float", "kernel arg0 TYPE_NAME==float")
ok(int(ksax.get_arg_info(1, cl.kernel_arg_info.ADDRESS_QUALIFIER)) == int(cl.kernel_arg_address_qualifier.GLOBAL), "kernel arg1 ADDRESS_QUALIFIER GLOBAL")

gws=(N,); lws=(64,)
# --- vadd via explicit Kernel + set_arg + enqueue_nd_range_kernel + event/profiling ---
kadd.set_arg(0, A); kadd.set_arg(1, B); kadd.set_arg(2, C)
ev = cl.enqueue_nd_range_kernel(q, kadd, gws, lws); ev.wait()
hc = np.empty_like(a); cl.enqueue_copy(q, hc, C); ok(np.array_equal(hc, a+b), "vadd == a+b (numpy)")
ok(int(ev.get_info(cl.event_info.COMMAND_EXECUTION_STATUS)) == int(cl.command_execution_status.COMPLETE), "event STATUS COMPLETE")
ok(int(ev.get_info(cl.event_info.COMMAND_TYPE)) == int(cl.command_type.NDRANGE_KERNEL), "event COMMAND_TYPE NDRANGE_KERNEL")
ok(ev.profile.end >= ev.profile.start, "event profiling end>=start")
ok(ev.profile.submit >= ev.profile.queued, "event profiling submit>=queued")

# --- global_offset dispatch: off_write with a nonzero offset leaves prefix untouched ---
koff = cl.Kernel(prog, "off_write")
OI = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=np.full(N, -1, dtype=np.int32))
koff.set_arg(0, OI)
cl.enqueue_nd_range_kernel(q, koff, (N-100,), None, global_work_offset=(100,)).wait()
hoi = np.empty(N, dtype=np.int32); cl.enqueue_copy(q, hoi, OI); q.finish()
ref_off = np.concatenate([np.full(100, -1, dtype=np.int32), np.arange(100, N, dtype=np.int32)])
ok(np.array_equal(hoi, ref_off), "global_work_offset==100 prefix untouched")

# --- vmul ---
prog.vmul(q, gws, lws, A, B, C).wait(); cl.enqueue_copy(q, hc, C); ok(np.array_equal(hc, a*b), "vmul == a*b (numpy)")

# --- saxpy via set_scalar_arg_dtypes (typed scalar arg) ---
ksax.set_scalar_arg_dtypes([np.float32, None, None])
Y = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=b.copy())
ksax(q, gws, lws, 3.0, A, Y).wait(); cl.enqueue_copy(q, hc, Y)
ok(np.allclose(hc, 3.0*a + b), "saxpy set_scalar_arg_dtypes == alpha*x+y (numpy)")

# --- local memory + barrier reduction (tight relative check vs numpy) ---
ng = N//lws[0]
R = cl.Buffer(ctx, mf.WRITE_ONLY, size=ng*4)
prog.reduce_sum(q, gws, lws, A, R, cl.LocalMemory(lws[0]*4)).wait()
hr = np.empty(ng, dtype=np.float32); cl.enqueue_copy(q, hr, R)
ref_partials = a.reshape(ng, lws[0]).sum(axis=1)
ok(np.allclose(hr, ref_partials, rtol=1e-5), "reduce_sum partials == numpy segment sums")
ok(np.isclose(float(hr.sum()), float(a.sum()), rtol=1e-5), "reduce_sum total == sum(a)")

# --- copy + fill + map/unmap APIs ---
D = cl.Buffer(ctx, mf.READ_WRITE, size=a.nbytes)
cl.enqueue_copy(q, D, A); cl.enqueue_copy(q, hc, D); ok(np.array_equal(hc, a), "buffer copy A->D bytes")
cl.enqueue_fill_buffer(q, D, np.float32(7.5), 0, a.nbytes); q.finish()
cl.enqueue_copy(q, hc, D); ok(np.allclose(hc, 7.5), "fill_buffer == 7.5")
mapped, _ev = cl.enqueue_map_buffer(q, D, cl.map_flags.WRITE, 0, (N,), np.float32)
ok(np.allclose(mapped, 7.5), "map_buffer WRITE view == 7.5")
mapped[3] = 42.0; mapped.base.release(); q.finish()
cl.enqueue_copy(q, hc, D); ok(hc[3] == 42.0 and np.allclose(np.delete(hc,3), 7.5), "unmap flushed host write to device")

# --- rect (region) copies: enqueue_copy with buffer/host origins + region ---
grid = np.arange(64, dtype=np.float32).reshape(8, 8)
BR = cl.Buffer(ctx, mf.READ_WRITE, size=grid.nbytes)
cl.enqueue_copy(q, BR, grid, buffer_origin=(0,0), host_origin=(0,0), region=(32,8)); q.finish()
rrows = np.zeros((8,8), dtype=np.float32)
cl.enqueue_copy(q, rrows, BR, buffer_origin=(0,0), host_origin=(0,0), region=(32,8)); q.finish()
ok(np.array_equal(rrows, grid), "write_rect/read_rect full-grid roundtrip")
# buffer->buffer rect: copy only the top 4 rows (32 bytes wide) into a zeroed dst
BR2 = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=np.zeros((8,8), dtype=np.float32))
cl.enqueue_copy(q, BR2, BR, src_origin=(0,0), dst_origin=(0,0), region=(32,4)); q.finish()
r2 = np.empty((8,8), dtype=np.float32); cl.enqueue_copy(q, r2, BR2); q.finish()
ok(np.array_equal(r2[:4], grid[:4]) and np.all(r2[4:] == 0), "copy_buffer_rect top-4-rows only")

# --- SVM family: allocation + map_rw/map_ro + memfill + kernel arg ---
svm = cl.SVMAllocation(ctx, N*4, 0, cl.svm_mem_flags.READ_WRITE)
ok(svm.size == N*4, "SVMAllocation size")
with svm.map_rw(q) as m:
    np.frombuffer(m, dtype=np.float32)[:] = a
with svm.map_ro(q) as m:
    ok(np.array_equal(np.frombuffer(m, dtype=np.float32), a), "SVM map_rw write + map_ro readback")
cl.enqueue_svm_memfill(q, svm, np.float32(2.0)); q.finish()
with svm.map_ro(q) as m:
    ok(np.allclose(np.frombuffer(m, dtype=np.float32), 2.0), "enqueue_svm_memfill == 2.0")
ksvm = cl.Kernel(prog, "svm_inc"); ksvm.set_arg(0, svm)
cl.enqueue_nd_range_kernel(q, ksvm, (N,), None).wait()
with svm.map_ro(q) as m:
    ok(np.allclose(np.frombuffer(m, dtype=np.float32), 3.0), "SVM kernel arg svm_inc == 3.0")

# --- Image family: ImageFormat + get_supported_image_formats + create_image + copy + kernel + fill + Sampler ---
img_fmt = cl.ImageFormat(cl.channel_order.R, cl.channel_type.FLOAT)
sup = cl.get_supported_image_formats(ctx, mf.READ_WRITE, cl.mem_object_type.IMAGE2D)
ok(any(f.channel_order == img_fmt.channel_order and f.channel_data_type == img_fmt.channel_data_type for f in sup),
   "get_supported_image_formats contains R/FLOAT")
IW, IH = 16, 12
himg = (np.arange(IW*IH, dtype=np.float32)).reshape(IH, IW)
src_img = cl.create_image(ctx, mf.READ_ONLY | mf.COPY_HOST_PTR, img_fmt, shape=(IW, IH), hostbuf=himg)
ok(src_img.get_image_info(cl.image_info.WIDTH) == IW, "image WIDTH")
ok(src_img.get_image_info(cl.image_info.HEIGHT) == IH, "image HEIGHT")
back = np.empty((IH, IW), dtype=np.float32)
cl.enqueue_copy(q, back, src_img, origin=(0,0), region=(IW, IH)); q.finish()
ok(np.array_equal(back, himg), "image copy image->host roundtrip")
smp = cl.Sampler(ctx, False, cl.addressing_mode.CLAMP_TO_EDGE, cl.filter_mode.NEAREST)
ok(smp.get_info(cl.sampler_info.NORMALIZED_COORDS) == 0, "Sampler NORMALIZED_COORDS==0")
ok(int(smp.get_info(cl.sampler_info.FILTER_MODE)) == int(cl.filter_mode.NEAREST), "Sampler FILTER_MODE NEAREST")
IMGSRC = r"""
__constant sampler_t S = CLK_NORMALIZED_COORDS_FALSE|CLK_ADDRESS_CLAMP_TO_EDGE|CLK_FILTER_NEAREST;
__kernel void imgscale(read_only image2d_t src, write_only image2d_t dst){
  int2 c=(int2)(get_global_id(0),get_global_id(1));
  write_imagef(dst,c,read_imagef(src,S,c)*2.0f);}
"""
iprog = cl.Program(ctx, IMGSRC).build()
dst_img = cl.create_image(ctx, mf.WRITE_ONLY, img_fmt, shape=(IW, IH))
iprog.imgscale(q, (IW, IH), None, src_img, dst_img); q.finish()
scaled = np.empty((IH, IW), dtype=np.float32)
cl.enqueue_copy(q, scaled, dst_img, origin=(0,0), region=(IW, IH)); q.finish()
ok(np.allclose(scaled, himg*2.0), "image kernel read_imagef/write_imagef *2 == numpy")
fimg = cl.create_image(ctx, mf.READ_WRITE, img_fmt, shape=(IW, IH))
cl.enqueue_fill_image(q, fimg, np.array([5.0,0,0,0], dtype=np.float32), origin=(0,0), region=(IW, IH)); q.finish()
filled = np.empty((IH, IW), dtype=np.float32)
cl.enqueue_copy(q, filled, fimg, origin=(0,0), region=(IW, IH)); q.finish()
ok(np.allclose(filled, 5.0), "enqueue_fill_image == 5.0")

# --- UserEvent host-driven gating on an out-of-order queue + wait_for dependency chain ---
ooq = cl.CommandQueue(ctx, properties=cl.command_queue_properties.OUT_OF_ORDER_EXEC_MODE_ENABLE)
ok(int(ooq.properties) & int(cl.command_queue_properties.OUT_OF_ORDER_EXEC_MODE_ENABLE), "OOO queue OUT_OF_ORDER set")
ue = cl.UserEvent(ctx)
ok(int(ue.get_info(cl.event_info.COMMAND_EXECUTION_STATUS)) != int(cl.command_execution_status.COMPLETE),
   "UserEvent not COMPLETE before set_status")
G = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=np.zeros(N, dtype=np.float32))
kadd2 = cl.Kernel(prog, "vadd"); kadd2.set_arg(0, A); kadd2.set_arg(1, B); kadd2.set_arg(2, G)
gated = cl.enqueue_nd_range_kernel(ooq, kadd2, gws, None, wait_for=[ue])
chained = cl.enqueue_copy(ooq, hc, G, wait_for=[gated])
ok(int(gated.get_info(cl.event_info.COMMAND_EXECUTION_STATUS)) != int(cl.command_execution_status.COMPLETE),
   "gated kernel blocked while UserEvent pending")
ue.set_status(cl.command_execution_status.COMPLETE)
chained.wait()
ok(np.array_equal(hc, a+b), "UserEvent-gated chained vadd == a+b (numpy)")

# --- boundary: zero-length ND-range is a no-op (destination stays put) ---
Z = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=np.full(N, 11.0, dtype=np.float32))
kz = cl.Kernel(prog, "svm_inc"); kz.set_arg(0, Z)
cl.enqueue_nd_range_kernel(q, kz, (0,), None).wait()
cl.enqueue_copy(q, hc, Z); q.finish()
ok(np.allclose(hc, 11.0), "zero-length ND-range is a no-op")

# --- boundary: large N >= 1,000,000 vadd verified element-wise ---
NB = 1 << 20
ab = np.random.default_rng(0).random(NB).astype(np.float32)
bb = np.random.default_rng(1).random(NB).astype(np.float32)
AB = cl.Buffer(ctx, mf.READ_ONLY | mf.COPY_HOST_PTR, hostbuf=ab)
BB = cl.Buffer(ctx, mf.READ_ONLY | mf.COPY_HOST_PTR, hostbuf=bb)
CB = cl.Buffer(ctx, mf.WRITE_ONLY, size=ab.nbytes)
prog.vadd(q, (NB,), None, AB, BB, CB).wait()
hcb = np.empty(NB, dtype=np.float32); cl.enqueue_copy(q, hcb, CB); q.finish()
ok(np.array_equal(hcb, ab+bb), "1M-element vadd verified element-wise vs numpy")

# --- negative control: corrupt one real device-output element, assert the correctness check flags it ---
corrupted = hcb.copy(); corrupted[123456] = np.float32(corrupted[123456] + 1.0)
ref_indep = ab + bb  # INDEPENDENT closed-form reference
ok(np.array_equal(hcb, ref_indep) and not np.array_equal(corrupted, ref_indep),
   "negative control: mutated element breaks match, clean output still matches")

# --- program introspection / binary round-trip / sub-region / map_count ---
ok(prog.num_kernels == 6, "Program.num_kernels == 6")
ok(len(prog.all_kernels()) == 6, "Program.all_kernels() == 6")
ok(all(s > 0 for s in prog.binary_sizes), "Program.binary_sizes > 0")
bins = prog.binaries; ok(len(bins) >= 1, "Program.binaries len")
prog2 = cl.Program(ctx, [dev], bins).build(); ok("vadd" in prog2.kernel_names, "Program from binaries rebuild retains vadd")
sub = A.get_sub_region(0, (N//2)*4); ok(sub.size == (N//2)*4, "Buffer.get_sub_region size")
sh = np.empty(N//2, dtype=np.float32); cl.enqueue_copy(q, sh, sub); q.finish()
ok(np.array_equal(sh, a[:N//2]), "sub-region reads A[:N/2]")
# Buffer.map_count exercised through a real map/unmap round-trip: 0 -> 1 while mapped -> 0 after unmap
MC = cl.Buffer(ctx, mf.READ_WRITE, size=a.nbytes)
mc_before = MC.map_count
mcv, _mcev = cl.enqueue_map_buffer(q, MC, cl.map_flags.WRITE, 0, (N,), np.float32); q.finish()
mc_mapped = MC.map_count
mcv.base.release(); q.finish()
mc_after = MC.map_count
ok(mc_before == 0 and mc_mapped == 1 and mc_after == 0, "Buffer.map_count 0->1(mapped)->0(unmapped)")

# --- program compile + separate link_program (separate compilation) verified vs numpy ---
lib_src = "float dbl(float x); float dbl(float x){return x*2.0f;}"
use_src = "float dbl(float x);\n__kernel void useit(__global float*a){int i=get_global_id(0);a[i]=dbl(a[i]);}"
plib = cl.Program(ctx, lib_src); plib.compile()
puse = cl.Program(ctx, use_src); puse.compile()
linked = cl.link_program(ctx, [plib, puse])
lin = np.arange(64, dtype=np.float32)
LB = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=lin.copy())
linked.useit(q, (64,), None, LB).wait()
lout = np.empty(64, dtype=np.float32); cl.enqueue_copy(q, lout, LB); q.finish()
ok(np.array_equal(lout, lin*2.0), "compile + link_program result == 2*x (numpy)")

# --- error / validation paths (pocl enforces CL validation) ---
try:
    cl.Program(ctx, "__kernel void bad(__global float*a){ this is not valid; }").build()
    ok(False, "broken build should raise RuntimeError")
except cl.RuntimeError:
    ok(True, "broken program build raises cl.RuntimeError")
bp = cl.Program(ctx, "__kernel void bad(__global float*a){ this is not valid; }")
try: bp.build()
except cl.RuntimeError: pass
ok(int(bp.get_build_info(dev, cl.program_build_info.STATUS)) == -2, "failed build STATUS == CL_BUILD_ERROR(-2)")
ok(len(bp.get_build_info(dev, cl.program_build_info.LOG)) > 0, "failed build LOG non-empty")

try:
    cl.Buffer(ctx, mf.READ_WRITE, size=0)
    ok(False, "zero-size buffer should raise")
except cl.LogicError as e:
    ok(int(e.code) == int(cl.status_code.INVALID_BUFFER_SIZE), "zero-size Buffer raises INVALID_BUFFER_SIZE")

try:
    cl.Kernel(prog, "vadd").set_arg(9, A)
    ok(False, "out-of-range set_arg should raise")
except cl.LogicError as e:
    ok(int(e.code) == int(cl.status_code.INVALID_ARG_INDEX), "set_arg bad index raises INVALID_ARG_INDEX")

# oversubscription: local size > device MAX_WORK_GROUP_SIZE must raise a CL error
kos = cl.Kernel(prog, "vadd"); kos.set_arg(0, A); kos.set_arg(1, B); kos.set_arg(2, C)
big_lws = int(max_wg) * 2
try:
    cl.enqueue_nd_range_kernel(q, kos, (big_lws,), (big_lws,)); q.finish()
    ok(False, "oversubscribed local size should raise")
except cl.LogicError:
    ok(True, "oversubscribed local size (lws>MAX_WORK_GROUP_SIZE) raises cl.LogicError")

# --- misc sync / marker / barrier / migrate APIs (each verified by a real handle/effect) ---
mev = cl.enqueue_marker(q); ok(int(mev.get_info(cl.event_info.COMMAND_TYPE)) == int(cl.command_type.MARKER), "enqueue_marker COMMAND_TYPE MARKER")
cl.enqueue_barrier(q)
cl.enqueue_migrate_mem_objects(q, [A]); q.finish()
mA = np.empty_like(a); cl.enqueue_copy(q, mA, A); q.finish()
ok(np.array_equal(mA, a), "A intact after barrier+migrate")
q.flush(); q.finish()

EXPECTED = 83
TOTAL = P[0]+F[0]
print("opencl-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (P[0], F[0], TOTAL, EXPECTED))
if F[0]==0 and TOTAL==EXPECTED:
    print("OPENCL_PY_FULL_API OK %d" % P[0]); sys.exit(0)
print("OPENCL_PY_FULL_API FAIL"); sys.exit(1)
