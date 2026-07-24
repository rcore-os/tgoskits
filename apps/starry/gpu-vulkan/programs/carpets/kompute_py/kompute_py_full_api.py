#!/usr/bin/env python3
# kompute_py_full_api.py - Kompute (kp) Vulkan-python compute API carpet on lavapipe (llvmpipe).
# Enumerates the kp surface - Manager (device enumeration / properties / tensor / tensor_t /
# algorithm / sequence / destroy), Tensor (numpy round-trip / size / type / init state / destroy),
# Algorithm (spirv + workgroup + spec/push constants / destroy), Sequence (record / eval /
# eval_async+eval_await / clear / rerecord / destroy / timestamps), and the
# Operations (OpTensorSyncDevice, OpAlgoDispatch, OpTensorSyncLocal, OpTensorCopy, OpMult) - and
# checks every compute result element-wise against an independent numpy reference, every queried
# property against a known value, and the one Python-level exception this binding actually raises.
# Prints "KOMPUTE_PY_FULL_API OK <n>" only when every assertion passes and the count equals the
# pinned EXPECTED total.
#
# kp exposes no compile helper (kp.Shader.compile_source is absent), so GLSL compute shaders are
# compiled to SPIR-V here with the host glslangValidator into a tempfile and read back as the bytes
# that mgr.algorithm(...) takes. The device is Mesa lavapipe: a software Vulkan queue with NO
# validation layer, so this carpet never asserts an error the driver would only raise under
# validation; the single exercised exception (Tensor.data_type returning an unregistered pybind enum)
# is a real Python-level TypeError from this build, and boundary cases the driver silently permits are
# asserted as PERMITTED or recorded as NON-COUNTING skips.
import sys, os, subprocess, tempfile, numpy as np, kp

P = [0]; F = [0]
def ok(c, d):
    if c: P[0] += 1
    else: F[0] += 1; sys.stderr.write("FAIL: %s\n" % d)

def skip(d):
    sys.stderr.write("SKIP: %s\n" % d)

def compile_spirv(glsl):
    with tempfile.TemporaryDirectory() as td:
        comp = os.path.join(td, "s.comp"); spv = os.path.join(td, "s.spv")
        with open(comp, "w") as fh: fh.write(glsl)
        subprocess.run(["glslangValidator", "-V", comp, "-o", spv],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        with open(spv, "rb") as fh: return fh.read()

VADD_GLSL = """#version 450
layout(local_size_x=256) in;
layout(set=0,binding=0) readonly  buffer A { float a[]; };
layout(set=0,binding=1) readonly  buffer B { float b[]; };
layout(set=0,binding=2) writeonly buffer C { float c[]; };
void main(){ uint i=gl_GlobalInvocationID.x; if(i<a.length()) c[i]=a[i]+b[i]; }
"""
SAXPY_GLSL = """#version 450
layout(local_size_x=256) in;
layout(set=0,binding=0) readonly  buffer A { float a[]; };
layout(set=0,binding=1) readonly  buffer B { float b[]; };
layout(set=0,binding=2) writeonly buffer C { float c[]; };
layout(push_constant) uniform Push { float alpha; } pc;
void main(){ uint i=gl_GlobalInvocationID.x; if(i<a.length()) c[i]=pc.alpha*a[i]+b[i]; }
"""
MUL_GLSL = """#version 450
layout(local_size_x=256) in;
layout(set=0,binding=0) readonly  buffer A { float a[]; };
layout(set=0,binding=1) readonly  buffer B { float b[]; };
layout(set=0,binding=2) writeonly buffer C { float c[]; };
void main(){ uint i=gl_GlobalInvocationID.x; if(i<a.length()) c[i]=a[i]*b[i]; }
"""
SCALE_GLSL = """#version 450
layout(local_size_x=256) in;
layout(constant_id=0) const float SCALE = 1.0;
layout(set=0,binding=0) readonly  buffer A { float a[]; };
layout(set=0,binding=1) writeonly buffer C { float c[]; };
void main(){ uint i=gl_GlobalInvocationID.x; if(i<a.length()) c[i]=SCALE*a[i]; }
"""
REDUCE_GLSL = """#version 450
layout(local_size_x=256) in;
layout(set=0,binding=0) readonly  buffer A { float a[]; };
layout(set=0,binding=1) writeonly buffer O { float o[]; };
shared float s[256];
void main(){
  uint lid=gl_LocalInvocationID.x; uint gid=gl_GlobalInvocationID.x;
  s[lid] = (gid < a.length()) ? a[gid] : 0.0;
  barrier();
  for(uint stride=128u; stride>0u; stride>>=1u){
    if(lid<stride) s[lid]+=s[lid+stride];
    barrier();
  }
  if(lid==0u) o[gl_WorkGroupID.x]=s[0];
}
"""

VADD_SPV   = compile_spirv(VADD_GLSL)
SAXPY_SPV  = compile_spirv(SAXPY_GLSL)
MUL_SPV    = compile_spirv(MUL_GLSL)
SCALE_SPV  = compile_spirv(SCALE_GLSL)
REDUCE_SPV = compile_spirv(REDUCE_GLSL)
ok(len(VADD_SPV) > 0 and len(VADD_SPV) % 4 == 0, "vadd SPIR-V is a non-empty word-aligned blob")
ok(VADD_SPV[:4] == b"\x03\x02\x23\x07", "vadd SPIR-V little-endian magic 0x07230203")
ok(len({VADD_SPV, SAXPY_SPV, MUL_SPV, SCALE_SPV, REDUCE_SPV}) == 5,
   "five distinct SPIR-V blobs, one per shader source")

def wg(n): return [(n + 255) // 256, 1, 1]

N = 4096
rng = np.random.default_rng(20260713)
a = rng.random(N, dtype=np.float32)
b = rng.random(N, dtype=np.float32)

# --- Manager: construction + device enumeration + properties -----------------------------------
mgr = kp.Manager()
mgr0 = kp.Manager(0)                     # explicit physical-device-index overload

devices = mgr.list_devices()
ok(isinstance(devices, list) and len(devices) >= 1, "list_devices returns >=1 device")
dev0 = devices[0]
ok(dev0["device_name"].startswith("llvmpipe"),
   "device 0 name is a llvmpipe software queue: %r" % dev0["device_name"])

props = mgr.get_device_properties()
ok(props["device_name"] == dev0["device_name"], "get_device_properties name matches list_devices")
ok(props["max_work_group_invocations"] >= 128,
   "max_work_group_invocations reported (%d)" % props["max_work_group_invocations"])
mwgs = props["max_work_group_size"]
ok(len(mwgs) == 3 and mwgs[0] >= 256, "max_work_group_size is a 3-tuple with x>=256: %r" % (mwgs,))
mwgc = props["max_work_group_count"]
ok(len(mwgc) == 3 and mwgc[0] >= 65535, "max_work_group_count is a 3-tuple with x>=65535: %r" % (mwgc,))
ok(props["timestamps_supported"] is True, "timestamps_supported advertised by lavapipe")
# workgroup we use for saxpy/scale (256 local x) must fit the reported invocation limit
ok(256 <= props["max_work_group_invocations"], "chosen local_size_x=256 fits invocation limit")

# --- Tensor: numpy round-trip / size / type / init --------------------------------------------
ta = mgr.tensor(a)
tb = mgr.tensor(b)
ok(ta.size() == N, "tensor.size() equals element count")
ok(ta.is_init() is True, "tensor is initialized after creation")
ok(ta.tensor_type() == kp.TensorTypes.device, "default tensor_type is device")
ok(np.array_equal(ta.data(), a), "tensor.data() round-trips the source array exactly")
ok(ta.data().dtype == np.float32, "tensor.data() dtype is float32")
th = mgr.tensor(a, kp.TensorTypes.host)
ok(th.tensor_type() == kp.TensorTypes.host, "host tensor_type honoured")
tstore = mgr.tensor(np.zeros(8, dtype=np.float32), kp.TensorTypes.storage)
ok(tstore.tensor_type() == kp.TensorTypes.storage, "storage tensor_type honoured")
ok(int(kp.TensorTypes.device) == 0 and int(kp.TensorTypes.host) == 1 and int(kp.TensorTypes.storage) == 2,
   "TensorTypes enum values device=0 host=1 storage=2")
# integer input is materialised as float32 by the binding
ti = mgr.tensor(np.array([2, 4, 6], dtype=np.int32))
ok(ti.data().dtype == np.float32 and np.array_equal(ti.data(), [2.0, 4.0, 6.0]),
   "int32 input tensor is stored as float32 with equal values")

# Tensor.data_type: this build returns an unregistered pybind enum -> real Python TypeError
try:
    ta.data_type()
    ok(False, "Tensor.data_type expected to raise on unregistered return type")
except TypeError:
    ok(True, "Tensor.data_type raises TypeError (unregistered pybind enum in this build)")

# --- Sequence: init / state flags --------------------------------------------------------------
seq = mgr.sequence()
ok(seq.is_init() is True, "sequence is initialized")
ok(seq.is_recording() is False, "fresh sequence is not recording")
ok(seq.is_running() is False, "fresh sequence is not running")

# --- vadd: c = a + b, checked element-wise vs numpy --------------------------------------------
tc = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_add = mgr.algorithm([ta, tb, tc], VADD_SPV, wg(N))
ok(algo_add.is_init() is True, "vadd algorithm is initialized")
ok(len(algo_add.get_tensors()) == 3, "vadd algorithm holds its 3 bound tensors")
seq.record(kp.OpTensorSyncDevice([ta, tb, tc])).eval()
seq.record(kp.OpAlgoDispatch(algo_add)).eval()
seq.record(kp.OpTensorSyncLocal([tc])).eval()
c_dev = tc.data().copy()
ref_add = a + b
ok(np.array_equal(c_dev, ref_add), "vadd result equals a+b for every element")

# --- NEGATIVE CONTROL: the comparator must reject a wrong reference and a single corrupted output
wrong_ref = (2.0 * a + b).astype(np.float32)
ok(not np.allclose(c_dev, wrong_ref), "negative control: a+b output differs from wrong ref 2a+b")
corrupt = c_dev.copy(); corrupt[777] += np.float32(1.0)
ok(not np.allclose(corrupt, ref_add), "negative control: one corrupted element detected vs a+b")
ok(np.allclose(c_dev, ref_add), "negative control: untouched output still matches a+b")

# --- saxpy with a PUSH CONSTANT alpha: c = alpha*a + b -----------------------------------------
def run_saxpy(alpha):
    tcx = mgr.tensor(np.zeros(N, dtype=np.float32))
    algo = mgr.algorithm([ta, tb, tcx], SAXPY_SPV, wg(N), [], [float(alpha)])
    s = mgr.sequence()
    s.record(kp.OpTensorSyncDevice([ta, tb, tcx])).eval()
    s.record(kp.OpAlgoDispatch(algo, [float(alpha)])).eval()
    s.record(kp.OpTensorSyncLocal([tcx])).eval()
    return tcx.data().copy()

sax25 = run_saxpy(2.5)
ok(np.allclose(sax25, 2.5 * a + b), "saxpy(alpha=2.5) equals 2.5a+b element-wise")
sax70 = run_saxpy(7.0)
ok(np.allclose(sax70, 7.0 * a + b), "saxpy(alpha=7.0) equals 7.0a+b element-wise")
ok(not np.allclose(sax25, sax70), "push-constant alpha changes the result (2.5 vs 7.0 differ)")
# OpAlgoDispatch push-constant override: dispatch with a third alpha on the alpha=7.0 algorithm
tc_ov = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_ov = mgr.algorithm([ta, tb, tc_ov], SAXPY_SPV, wg(N), [], [7.0])
s_ov = mgr.sequence()
s_ov.record(kp.OpTensorSyncDevice([ta, tb, tc_ov])).eval()
s_ov.record(kp.OpAlgoDispatch(algo_ov, [3.0])).eval()   # override 7.0 -> 3.0 at dispatch
s_ov.record(kp.OpTensorSyncLocal([tc_ov])).eval()
ok(np.allclose(tc_ov.data(), 3.0 * a + b), "OpAlgoDispatch push-constant override applies alpha=3.0")

# --- elementwise multiply via a custom shader + OpAlgoDispatch ---------------------------------
tc_mul = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_mul = mgr.algorithm([ta, tb, tc_mul], MUL_SPV, wg(N))
sm = mgr.sequence()
sm.record(kp.OpTensorSyncDevice([ta, tb, tc_mul])).eval()
sm.record(kp.OpAlgoDispatch(algo_mul)).eval()
sm.record(kp.OpTensorSyncLocal([tc_mul])).eval()
ok(np.allclose(tc_mul.data(), a * b), "multiply shader result equals a*b element-wise")

# --- OpMult built-in operation (overrides shader to a*b) ---------------------------------------
tc_opmul = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_om = mgr.algorithm([ta, tb, tc_opmul], VADD_SPV)   # OpMult overrides the shader data
so = mgr.sequence()
so.record(kp.OpTensorSyncDevice([ta, tb, tc_opmul])).eval()
so.record(kp.OpMult([ta, tb, tc_opmul], algo_om)).eval()
so.record(kp.OpTensorSyncLocal([tc_opmul])).eval()
ok(np.allclose(tc_opmul.data(), a * b), "OpMult built-in result equals a*b element-wise")

# --- shared-memory workgroup reduction: per-group partial sums, folded on the host --------------
ngroups = (N + 255) // 256
tr_in = mgr.tensor(a)
tr_out = mgr.tensor(np.zeros(ngroups, dtype=np.float32))
algo_red = mgr.algorithm([tr_in, tr_out], REDUCE_SPV, [ngroups, 1, 1])
sr = mgr.sequence()
sr.record(kp.OpTensorSyncDevice([tr_in, tr_out])).eval()
sr.record(kp.OpAlgoDispatch(algo_red)).eval()
sr.record(kp.OpTensorSyncLocal([tr_out])).eval()
partials = tr_out.data()
ref_partials = a.reshape(ngroups, 256).sum(axis=1)
ok(tr_out.size() == ngroups, "reduction emits one partial per 256-wide workgroup")
ok(np.allclose(partials, ref_partials, rtol=1e-4), "per-group partial sums match numpy per group")
ok(np.isclose(float(partials.sum()), float(a.sum()), rtol=1e-4), "folded partials equal numpy total sum")

# --- spec constant (constant_id=0) baked at algorithm-build time --------------------------------
def run_scale(scale):
    tcx = mgr.tensor(np.zeros(N, dtype=np.float32))
    algo = mgr.algorithm([ta, tcx], SCALE_SPV, wg(N), [float(scale)], [])
    s = mgr.sequence()
    s.record(kp.OpTensorSyncDevice([ta, tcx])).eval()
    s.record(kp.OpAlgoDispatch(algo)).eval()
    s.record(kp.OpTensorSyncLocal([tcx])).eval()
    return tcx.data().copy()

sc3 = run_scale(3.0)
ok(np.allclose(sc3, 3.0 * a), "spec-constant SCALE=3.0 yields 3.0*a element-wise")
sc9 = run_scale(9.0)
ok(np.allclose(sc9, 9.0 * a), "spec-constant SCALE=9.0 yields 9.0*a element-wise")
ok(not np.allclose(sc3, sc9), "spec constant changes the result (3.0 vs 9.0 differ)")

# --- OpTensorCopy: device-side copy replicates the source exactly ------------------------------
td = mgr.tensor(np.zeros(N, dtype=np.float32))
sc = mgr.sequence()
sc.record(kp.OpTensorSyncDevice([ta, td])).eval()
sc.record(kp.OpTensorCopy([ta, td])).eval()
sc.record(kp.OpTensorSyncLocal([td])).eval()
ok(np.array_equal(td.data(), a), "OpTensorCopy replicates source tensor exactly")

# --- eval_async + eval_await run the same dispatch without an inline barrier -------------------
tc_as = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_as = mgr.algorithm([ta, tb, tc_as], VADD_SPV, wg(N))
sa = mgr.sequence()
sa.record(kp.OpTensorSyncDevice([ta, tb, tc_as])).eval()
sa.record(kp.OpAlgoDispatch(algo_as))
sa.eval_async()
sa.eval_await()
sa.record(kp.OpTensorSyncLocal([tc_as])).eval()
ok(np.array_equal(tc_as.data(), a + b), "eval_async+eval_await path yields a+b element-wise")
ok(sa.is_running() is False, "sequence not running after eval_await completes")

# --- multi-op record in a single sequence, one eval -------------------------------------------
tc_multi = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_multi = mgr.algorithm([ta, tb, tc_multi], VADD_SPV, wg(N))
sms = mgr.sequence()
sms.record(kp.OpTensorSyncDevice([ta, tb, tc_multi]))
sms.record(kp.OpAlgoDispatch(algo_multi))
sms.record(kp.OpTensorSyncLocal([tc_multi]))
sms.eval()
ok(np.array_equal(tc_multi.data(), a + b), "batched record(sync/dispatch/sync)+single eval yields a+b")

# --- timestamps: a timestamp-latching sequence returns real GPU counters -----------------------
# A timestamp sequence latches one counter before the batch plus one per recorded op; it must be
# recorded once and evaluated once (repeated eval cycles overflow the query pool on lavapipe). Three
# ops therefore yield four counters.
seq_ts = mgr.sequence(0, 8)
tc_ts = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_ts = mgr.algorithm([ta, tb, tc_ts], VADD_SPV, wg(N))
seq_ts.record(kp.OpTensorSyncDevice([ta, tb, tc_ts]))
seq_ts.record(kp.OpAlgoDispatch(algo_ts))
seq_ts.record(kp.OpTensorSyncLocal([tc_ts]))
seq_ts.eval()
ts = seq_ts.get_timestamps()
ok(isinstance(ts, list) and len(ts) == 4, "timestamp sequence latches 4 counters for 3 recorded ops")
ok(all(isinstance(t, int) and t > 0 for t in ts), "latched timestamps are positive integer counters")
ok(np.array_equal(tc_ts.data(), a + b), "timestamped dispatch still computes a+b correctly")

# --- boundary: >=100000-element dispatch checked element-wise vs numpy -------------------------
BIG = 131072
xa = np.arange(BIG, dtype=np.float32)
xb = (2.0 * np.arange(BIG) + 1.0).astype(np.float32)
tbx_a = mgr.tensor(xa); tbx_b = mgr.tensor(xb); tbx_c = mgr.tensor(np.zeros(BIG, dtype=np.float32))
algo_big = mgr.algorithm([tbx_a, tbx_b, tbx_c], VADD_SPV, wg(BIG))
sb = mgr.sequence()
sb.record(kp.OpTensorSyncDevice([tbx_a, tbx_b, tbx_c])).eval()
sb.record(kp.OpAlgoDispatch(algo_big)).eval()
sb.record(kp.OpTensorSyncLocal([tbx_c])).eval()
ok(tbx_c.size() == BIG, "boundary tensor holds %d elements" % BIG)
ok(np.array_equal(tbx_c.data(), xa + xb), "boundary %d-element vadd matches numpy element-wise" % BIG)
ok(tbx_c.data()[BIG - 1] == xa[BIG - 1] + xb[BIG - 1], "boundary last element computed (no tail drop)")

# --- boundary: minimal 1-element dispatch, workgroup (1,1,1) -----------------------------------
tm_a = mgr.tensor(np.array([9.0], dtype=np.float32))
tm_b = mgr.tensor(np.array([4.0], dtype=np.float32))
tm_c = mgr.tensor(np.array([0.0], dtype=np.float32))
algo_min = mgr.algorithm([tm_a, tm_b, tm_c], VADD_SPV, [1, 1, 1])
smin = mgr.sequence()
smin.record(kp.OpTensorSyncDevice([tm_a, tm_b, tm_c])).eval()
smin.record(kp.OpAlgoDispatch(algo_min)).eval()
smin.record(kp.OpTensorSyncLocal([tm_c])).eval()
ok(tm_c.data()[0] == 13.0, "minimal 1-element workgroup(1,1,1) dispatch computes 9+4=13")

# --- Manager.tensor_t: typed-tensor helper round-trip (float32 default) ------------------------
# tensor_t is the templated typed constructor; on this build it materialises the same float32
# device tensor as mgr.tensor and round-trips the numpy source exactly.
tt = mgr.tensor_t(a)
ok(tt.size() == N, "tensor_t typed helper sizes to element count")
ok(tt.is_init() is True, "tensor_t tensor is initialized")
ok(tt.tensor_type() == kp.TensorTypes.device, "tensor_t default tensor_type is device")
ok(np.array_equal(tt.data(), a), "tensor_t.data() round-trips the source array exactly")
ok(tt.data().dtype == np.float32, "tensor_t tensor dtype is float32")

# --- Sequence.clear(): drops recorded ops, leaves the sequence reusable -------------------------
# clear() frees the command buffer and returns the sequence to a non-recording, non-running idle
# state; a subsequent record/eval on the SAME sequence must recompute correctly (proving the
# handle survived and was genuinely re-recorded, not replaying a stale command buffer).
tcl = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_cl = mgr.algorithm([ta, tb, tcl], VADD_SPV, wg(N))
scl = mgr.sequence()
scl.record(kp.OpTensorSyncDevice([ta, tb, tcl])).eval()
scl.record(kp.OpAlgoDispatch(algo_cl)).eval()
scl.record(kp.OpTensorSyncLocal([tcl])).eval()
ok(np.array_equal(tcl.data(), a + b), "pre-clear sequence computed a+b")
scl.clear()
ok(scl.is_recording() is False, "sequence is not recording after clear()")
ok(scl.is_running() is False, "sequence is not running after clear()")
tcl2 = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_cl2 = mgr.algorithm([ta, tb, tcl2], MUL_SPV, wg(N))   # different op -> a*b, not a+b
scl.record(kp.OpTensorSyncDevice([ta, tb, tcl2])).eval()
scl.record(kp.OpAlgoDispatch(algo_cl2)).eval()
scl.record(kp.OpTensorSyncLocal([tcl2])).eval()
ok(np.allclose(tcl2.data(), a * b) and not np.allclose(tcl2.data(), a + b),
   "cleared sequence re-records a fresh op (a*b), not the pre-clear a+b batch")

# --- Sequence.rerecord(): re-emits the same saved ops; deterministic re-execution ---------------
# rerecord() clears the command buffer and re-records the operations already saved on the sequence,
# then a second eval must reproduce the identical result bit-for-bit (used when underlying tensors
# or algorithms were mutated in place).
tre = mgr.tensor(np.zeros(N, dtype=np.float32))
algo_re = mgr.algorithm([ta, tb, tre], SAXPY_SPV, wg(N), [], [2.5])
sre = mgr.sequence()
sre.record(kp.OpTensorSyncDevice([ta, tb, tre]))
sre.record(kp.OpAlgoDispatch(algo_re, [2.5]))
sre.record(kp.OpTensorSyncLocal([tre]))
sre.eval()
re_first = tre.data().copy()
ok(np.allclose(re_first, 2.5 * a + b), "pre-rerecord saxpy(2.5) computed 2.5a+b")
sre.rerecord()
sre.eval()
ok(np.array_equal(tre.data(), re_first),
   "rerecord()+eval reproduces the saved ops bit-for-bit (deterministic re-execution)")

# --- destroy() lifecycle: an initialized resource reports not-init after destroy ---------------
td_gone = mgr.tensor(np.zeros(4, dtype=np.float32))
ok(td_gone.is_init() is True, "tensor init before destroy")
td_gone.destroy()
ok(td_gone.is_init() is False, "tensor reports not-init after destroy()")

# Algorithm.destroy(): explicit GPU-resource release flips is_init True -> False
algo_kill = mgr.algorithm([ta, tb, tcl], VADD_SPV, wg(N))
ok(algo_kill.is_init() is True, "algorithm init before destroy")
algo_kill.destroy()
ok(algo_kill.is_init() is False, "algorithm reports not-init after destroy()")

# Sequence.destroy(): frees the command buffer/pool and sets init False
seq_kill = mgr.sequence()
seq_kill.record(kp.OpTensorSyncDevice([ta])).eval()
ok(seq_kill.is_init() is True, "sequence init before destroy")
seq_kill.destroy()
ok(seq_kill.is_init() is False, "sequence reports not-init after destroy()")

# Manager.destroy(): tears down the device and every tensor it still manages -> managed tensor
# reports not-init. Run on a throwaway manager so the primary mgr keeps serving the summary path.
mgr_kill = kp.Manager()
tk = mgr_kill.tensor(np.zeros(4, dtype=np.float32))
ok(tk.is_init() is True, "manager-owned tensor init before manager destroy")
mgr_kill.destroy()
ok(tk.is_init() is False, "manager.destroy() releases its managed tensor (not-init)")

EXPECTED = 72
TOTAL = P[0] + F[0]
print("kompute-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (P[0], F[0], TOTAL, EXPECTED))
if F[0] == 0 and TOTAL == EXPECTED:
    print("KOMPUTE_PY_FULL_API OK %d" % P[0]); sys.exit(0)
print("KOMPUTE_PY_FULL_API FAIL"); sys.exit(1)
