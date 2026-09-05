// kompute_cpp_full_api.cpp - Kompute (libkompute C++) compute API carpet on lavapipe (llvmpipe).
// Drives the real Kompute C++ surface - kp::Manager (construction / device enumeration / properties /
// tensor / tensorT / algorithm / sequence / clear / destroy), kp::Tensor (vector round-trip / size /
// dataType / tensorType / isInit / destroy), kp::Algorithm (spirv + workgroup + spec/push constants /
// getTensors / getWorkgroup / getPushConstants / isInit / destroy), kp::Sequence (record / eval /
// evalAsync+evalAwait / clear / rerecord / isRunning / isRecording / destroy / getTimestamps) and the
// operations (OpTensorSyncDevice / OpAlgoDispatch (+push-constant override) / OpTensorSyncLocal /
// OpTensorCopy) - and checks every compute result element-wise against an independent closed-form
// reference, every queried property against a known value, and the resource lifecycle transitions.
// Prints "KOMPUTE_CPP_FULL_API OK <n>" only when every assertion passes and count==EXPECTED.
//
// Kompute drives Vulkan through vk::DynamicLoader (VULKAN_HPP_DEFAULT_DISPATCHER), so this is a
// dynamically linked musl binary that dlopen()s libvulkan.so.1 at runtime - the same dynamic-link /
// dlopen path StarryOS supports (only fully static musl binaries stub dlopen). The device is Mesa
// lavapipe: a software Vulkan queue with NO validation layer, so this carpet never asserts an error
// the driver would only raise under validation; boundary cases the driver silently permits are
// asserted as computed correctly rather than faking a rejection. SPIR-V is precompiled by glslc in
// prebuild and loaded from kompute_shaders/*.spv (Kompute's algorithm() takes the SPIR-V words
// directly, so no shaderc/glslang is linked into the cell).
#include <kompute/Manager.hpp>
#include <kompute/Tensor.hpp>
#include <kompute/Algorithm.hpp>
#include <kompute/Sequence.hpp>
#include <kompute/operations/OpTensorSyncDevice.hpp>
#include <kompute/operations/OpTensorSyncLocal.hpp>
#include <kompute/operations/OpAlgoDispatch.hpp>
#include <kompute/operations/OpTensorCopy.hpp>
#include <cstdio>
#include <cstdint>
#include <cmath>
#include <cstring>
#include <vector>
#include <fstream>
#include <memory>
#include <string>

static int PASS = 0, FAIL = 0;
static void ok(bool c, const char* d) { if (c) PASS++; else { FAIL++; fprintf(stderr, "FAIL: %s\n", d); } }

// element-wise float comparison with a relative tolerance, matching the sibling cells.
static bool feq(float a, float b) { return std::fabs(a - b) <= 1e-4f * (1.0f + std::fabs(b)); }
static bool veq(const std::vector<float>& x, const std::vector<float>& y) {
    if (x.size() != y.size()) return false;
    for (size_t i = 0; i < x.size(); i++) if (!feq(x[i], y[i])) return false;
    return true;
}

static std::vector<uint32_t> load_spv(const char* p) {
    std::ifstream f(p, std::ios::binary | std::ios::ate);
    if (!f) return {};
    std::streamsize n = f.tellg(); f.seekg(0);
    std::vector<uint32_t> b(n / 4);
    f.read(reinterpret_cast<char*>(b.data()), n);
    return b;
}

// deterministic LCG so the reference data is reproducible bit-for-bit (seed 0x233), independent of
// any host RNG. Values in [0,1) as float32.
struct Rng {
    uint64_t s;
    explicit Rng(uint64_t seed) : s(seed) {}
    float next() {
        s = s * 6364136223846793005ULL + 1442695040888963407ULL;
        uint32_t hi = static_cast<uint32_t>(s >> 40); // top 24 bits
        return static_cast<float>(hi) / static_cast<float>(1u << 24);
    }
};

static kp::Workgroup wg(uint32_t n) { return kp::Workgroup{ (n + 255u) / 256u, 1u, 1u }; }

int main() {
    const uint32_t N = 4096;

    auto vadd_spv   = load_spv("kompute_shaders/vadd.spv");
    auto saxpy_spv  = load_spv("kompute_shaders/saxpy.spv");
    auto mul_spv    = load_spv("kompute_shaders/mul.spv");
    auto scale_spv  = load_spv("kompute_shaders/scale.spv");
    auto reduce_spv = load_spv("kompute_shaders/reduce.spv");
    ok(vadd_spv.size() > 0 && vadd_spv[0] == 0x07230203u, "vadd SPIR-V loaded, little-endian magic word");
    ok(saxpy_spv.size() > 0 && mul_spv.size() > 0 && scale_spv.size() > 0 && reduce_spv.size() > 0,
       "all five precompiled SPIR-V blobs loaded non-empty");
    ok(vadd_spv != saxpy_spv && vadd_spv != mul_spv && saxpy_spv != scale_spv && mul_spv != reduce_spv,
       "distinct SPIR-V blobs, one per shader source");

    // --- reference data (seed 0x233) --------------------------------------------------------------
    Rng rng(0x233);
    std::vector<float> a(N), b(N);
    for (uint32_t i = 0; i < N; i++) a[i] = rng.next();
    for (uint32_t i = 0; i < N; i++) b[i] = rng.next();
    std::vector<float> ref_add(N), ref_mul(N);
    for (uint32_t i = 0; i < N; i++) { ref_add[i] = a[i] + b[i]; ref_mul[i] = a[i] * b[i]; }

    // --- Manager: construction + device enumeration + properties ----------------------------------
    kp::Manager mgr;
    kp::Manager mgr0(0); // explicit physical-device-index overload

    auto devices = mgr.listDevices();
    ok(devices.size() >= 1, "listDevices returns >=1 physical device");
    auto props = mgr.getDeviceProperties();
    std::string devName(props.deviceName.data());
    ok(devName.rfind("llvmpipe", 0) == 0, "device 0 name is a llvmpipe software queue");
    ok(props.apiVersion >= VK_MAKE_VERSION(1, 1, 0), "device advertises Vulkan >= 1.1");
    ok(props.limits.maxComputeWorkGroupInvocations >= 256,
       "maxComputeWorkGroupInvocations >= 256 (chosen local_size_x fits)");
    ok(props.limits.maxComputeWorkGroupSize[0] >= 256, "maxComputeWorkGroupSize.x >= 256");
    ok(props.limits.maxComputeWorkGroupCount[0] >= 65535, "maxComputeWorkGroupCount.x >= 65535");
    ok(props.limits.maxComputeSharedMemorySize >= 256 * sizeof(float),
       "maxComputeSharedMemorySize fits the 256-float reduction scratch");
    { auto inst = mgr.getVkInstance(); ok(inst && (bool)*inst, "getVkInstance returns a live instance"); }

    // --- Tensor: vector round-trip / size / type / init -------------------------------------------
    auto ta = mgr.tensor(a);
    auto tb = mgr.tensor(b);
    ok(ta->size() == N, "tensor.size() equals element count");
    ok(ta->isInit(), "tensor is initialized after creation");
    ok(ta->tensorType() == kp::Tensor::TensorTypes::eDevice, "default tensorType is eDevice");
    ok(ta->dataType() == kp::Tensor::TensorDataTypes::eFloat, "tensor dataType is eFloat");
    ok(veq(ta->vector(), a), "tensor.vector<float>() round-trips the source array exactly");
    auto th = mgr.tensor(a, kp::Tensor::TensorTypes::eHost);
    ok(th->tensorType() == kp::Tensor::TensorTypes::eHost, "eHost tensorType honoured");
    auto tstore = mgr.tensor(std::vector<float>(8, 0.0f), kp::Tensor::TensorTypes::eStorage);
    ok(tstore->tensorType() == kp::Tensor::TensorTypes::eStorage, "eStorage tensorType honoured");
    ok(static_cast<int>(kp::Tensor::TensorTypes::eDevice) == 0 &&
       static_cast<int>(kp::Tensor::TensorTypes::eHost) == 1 &&
       static_cast<int>(kp::Tensor::TensorTypes::eStorage) == 2,
       "TensorTypes enum values device=0 host=1 storage=2");
    // typed tensorT<float> helper materialises the same float32 device tensor and round-trips exactly
    auto tt = mgr.tensorT<float>(a);
    ok(tt->size() == N && tt->isInit(), "tensorT<float> sizes to element count and is initialized");
    ok(tt->tensorType() == kp::Tensor::TensorTypes::eDevice, "tensorT default tensorType is eDevice");
    ok(veq(tt->vector(), a), "tensorT.vector() round-trips the source array exactly");

    // --- Sequence: init / state flags -------------------------------------------------------------
    auto seq = mgr.sequence();
    ok(seq->isInit(), "sequence is initialized");
    ok(!seq->isRecording(), "fresh sequence is not recording");
    ok(!seq->isRunning(), "fresh sequence is not running");

    // --- vadd: c = a + b, checked element-wise vs closed form -------------------------------------
    auto tc = mgr.tensor(std::vector<float>(N, 0.0f));
    auto algo_add = mgr.algorithm({ ta, tb, tc }, vadd_spv, wg(N));
    ok(algo_add->isInit(), "vadd algorithm is initialized");
    ok(algo_add->getTensors().size() == 3, "vadd algorithm holds its 3 bound tensors");
    ok(algo_add->getWorkgroup()[0] == (N + 255u) / 256u, "vadd algorithm workgroup.x matches dispatch");
    seq->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tc });
    seq->eval<kp::OpAlgoDispatch>(algo_add);
    seq->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tc });
    auto c_dev = tc->vector();
    ok(veq(c_dev, ref_add), "vadd result equals a+b for every element");

    // --- NEGATIVE CONTROL: the comparator must reject a wrong reference and a single corrupted output
    std::vector<float> wrong_ref(N);
    for (uint32_t i = 0; i < N; i++) wrong_ref[i] = 2.0f * a[i] + b[i];
    ok(!veq(c_dev, wrong_ref), "negative control: a+b output differs from wrong ref 2a+b");
    { auto corrupt = c_dev; corrupt[777] += 1.0f;
      ok(!veq(corrupt, ref_add), "negative control: one corrupted element detected vs a+b"); }
    ok(veq(c_dev, ref_add), "negative control: untouched output still matches a+b");

    // --- saxpy with a PUSH CONSTANT alpha: c = alpha*a + b ----------------------------------------
    auto run_saxpy = [&](float alpha) {
        auto tcx = mgr.tensor(std::vector<float>(N, 0.0f));
        auto algo = mgr.algorithm({ ta, tb, tcx }, saxpy_spv, wg(N),
                                  std::vector<float>{}, std::vector<float>{ alpha });
        auto s = mgr.sequence();
        s->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tcx });
        s->eval<kp::OpAlgoDispatch>(algo);
        s->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tcx });
        return tcx->vector();
    };
    auto ref_saxpy = [&](float alpha) { std::vector<float> r(N); for (uint32_t i=0;i<N;i++) r[i]=alpha*a[i]+b[i]; return r; };
    auto sax25 = run_saxpy(2.5f);
    ok(veq(sax25, ref_saxpy(2.5f)), "saxpy(alpha=2.5) equals 2.5a+b element-wise");
    auto sax70 = run_saxpy(7.0f);
    ok(veq(sax70, ref_saxpy(7.0f)), "saxpy(alpha=7.0) equals 7.0a+b element-wise");
    ok(!veq(sax25, sax70), "push-constant alpha changes the result (2.5 vs 7.0 differ)");
    // OpAlgoDispatch push-constant OVERRIDE: build with alpha=7.0, dispatch with alpha=3.0 override
    { auto tc_ov = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_ov = mgr.algorithm({ ta, tb, tc_ov }, saxpy_spv, wg(N),
                                   std::vector<float>{}, std::vector<float>{ 7.0f });
      auto s_ov = mgr.sequence();
      s_ov->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tc_ov });
      s_ov->eval<kp::OpAlgoDispatch>(algo_ov, std::vector<float>{ 3.0f }); // override 7.0 -> 3.0
      s_ov->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tc_ov });
      ok(veq(tc_ov->vector(), ref_saxpy(3.0f)),
         "OpAlgoDispatch push-constant override applies alpha=3.0");
      // OpAlgoDispatch::record calls algorithm->setPushConstants(override), so after the dispatch the
      // algorithm's stored push constant reflects the last-dispatched override (3.0), not the 7.0 it
      // was built with - the real Kompute override mechanism, asserted against the observed value.
      ok(veq(algo_ov->getPushConstants<float>(), std::vector<float>{ 3.0f }),
         "algorithm's push constant reflects the last-dispatched override (alpha=3.0)"); }

    // --- element-wise multiply via a custom shader + OpAlgoDispatch --------------------------------
    { auto tc_mul = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_mul = mgr.algorithm({ ta, tb, tc_mul }, mul_spv, wg(N));
      auto sm = mgr.sequence();
      sm->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tc_mul });
      sm->eval<kp::OpAlgoDispatch>(algo_mul);
      sm->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tc_mul });
      ok(veq(tc_mul->vector(), ref_mul), "multiply shader result equals a*b element-wise"); }

    // --- shared-memory workgroup reduction: per-group partial sums, folded on the host ------------
    { uint32_t ngroups = (N + 255u) / 256u;
      auto tr_in = mgr.tensor(a);
      auto tr_out = mgr.tensor(std::vector<float>(ngroups, 0.0f));
      auto algo_red = mgr.algorithm({ tr_in, tr_out }, reduce_spv, kp::Workgroup{ ngroups, 1, 1 });
      auto sr = mgr.sequence();
      sr->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ tr_in, tr_out });
      sr->eval<kp::OpAlgoDispatch>(algo_red);
      sr->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tr_out });
      auto partials = tr_out->vector();
      std::vector<float> ref_partials(ngroups, 0.0f);
      for (uint32_t g = 0; g < ngroups; g++) for (uint32_t j = 0; j < 256; j++) ref_partials[g] += a[g*256+j];
      ok(tr_out->size() == ngroups, "reduction emits one partial per 256-wide workgroup");
      ok(veq(partials, ref_partials), "per-group partial sums match reference per group");
      double sp = 0, sa = 0; for (float v : partials) sp += v; for (float v : a) sa += v;
      ok(std::fabs(sp - sa) <= 1e-2 * (1.0 + std::fabs(sa)), "folded partials equal the reference total sum"); }

    // --- spec constant (constant_id=0) baked at algorithm-build time -------------------------------
    auto run_scale = [&](float scale) {
        auto tcx = mgr.tensor(std::vector<float>(N, 0.0f));
        auto algo = mgr.algorithm({ ta, tcx }, scale_spv, wg(N),
                                  std::vector<float>{ scale }, std::vector<float>{});
        auto s = mgr.sequence();
        s->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tcx });
        s->eval<kp::OpAlgoDispatch>(algo);
        s->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tcx });
        return tcx->vector();
    };
    auto ref_scale = [&](float sc) { std::vector<float> r(N); for (uint32_t i=0;i<N;i++) r[i]=sc*a[i]; return r; };
    auto sc3 = run_scale(3.0f);
    ok(veq(sc3, ref_scale(3.0f)), "spec-constant SCALE=3.0 yields 3.0*a element-wise");
    auto sc9 = run_scale(9.0f);
    ok(veq(sc9, ref_scale(9.0f)), "spec-constant SCALE=9.0 yields 9.0*a element-wise");
    ok(!veq(sc3, sc9), "spec constant changes the result (3.0 vs 9.0 differ)");

    // --- OpTensorCopy: device-side copy replicates the source exactly ------------------------------
    { auto td = mgr.tensor(std::vector<float>(N, 0.0f));
      auto s = mgr.sequence();
      s->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, td });
      s->eval<kp::OpTensorCopy>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, td });
      s->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ td });
      ok(veq(td->vector(), a), "OpTensorCopy replicates source tensor exactly"); }

    // --- record()+eval() batched in one sequence --------------------------------------------------
    { auto tc_b = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_b = mgr.algorithm({ ta, tb, tc_b }, vadd_spv, wg(N));
      auto s = mgr.sequence();
      s->record<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tc_b });
      s->record<kp::OpAlgoDispatch>(algo_b);
      s->record<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tc_b });
      s->eval();
      ok(veq(tc_b->vector(), ref_add), "batched record(sync/dispatch/sync)+single eval yields a+b"); }

    // --- evalAsync + evalAwait run the same dispatch asynchronously --------------------------------
    { auto tc_as = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_as = mgr.algorithm({ ta, tb, tc_as }, vadd_spv, wg(N));
      auto s = mgr.sequence();
      s->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tc_as });
      s->record<kp::OpAlgoDispatch>(algo_as);
      s->evalAsync();
      s->evalAwait();
      s->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tc_as });
      ok(veq(tc_as->vector(), ref_add), "evalAsync+evalAwait path yields a+b element-wise");
      ok(!s->isRunning(), "sequence not running after evalAwait completes"); }

    // --- timestamps: a timestamp-latching sequence returns real GPU counters ----------------------
    // A timestamp sequence latches one counter before the batch plus one per recorded op; recorded
    // once and evaluated once. Three ops therefore yield four counters.
    { auto seq_ts = mgr.sequence(0, 8);
      auto tc_ts = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_ts = mgr.algorithm({ ta, tb, tc_ts }, vadd_spv, wg(N));
      seq_ts->record<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tc_ts });
      seq_ts->record<kp::OpAlgoDispatch>(algo_ts);
      seq_ts->record<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tc_ts });
      seq_ts->eval();
      auto ts = seq_ts->getTimestamps();
      ok(ts.size() == 4, "timestamp sequence latches 4 counters for 3 recorded ops");
      bool allpos = true; for (auto t : ts) if (t == 0) allpos = false;
      ok(allpos, "latched timestamps are non-zero counters");
      ok(veq(tc_ts->vector(), ref_add), "timestamped dispatch still computes a+b correctly"); }

    // --- boundary: >=100000-element dispatch checked element-wise vs closed form -------------------
    { const uint32_t BIG = 131072;
      std::vector<float> xa(BIG), xb(BIG), xref(BIG);
      for (uint32_t i = 0; i < BIG; i++) { xa[i] = (float)i; xb[i] = 2.0f*(float)i + 1.0f; xref[i] = xa[i]+xb[i]; }
      auto tA = mgr.tensor(xa), tB = mgr.tensor(xb), tC = mgr.tensor(std::vector<float>(BIG, 0.0f));
      auto algo_big = mgr.algorithm({ tA, tB, tC }, vadd_spv, wg(BIG));
      auto s = mgr.sequence();
      s->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ tA, tB, tC });
      s->eval<kp::OpAlgoDispatch>(algo_big);
      s->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tC });
      auto cbig = tC->vector();
      ok(tC->size() == BIG, "boundary tensor holds 131072 elements");
      ok(veq(cbig, xref), "boundary 131072-element vadd matches reference element-wise");
      ok(feq(cbig[BIG-1], xa[BIG-1]+xb[BIG-1]), "boundary last element computed (no tail drop)"); }

    // --- boundary: minimal 1-element dispatch, workgroup (1,1,1) -----------------------------------
    { auto tm_a = mgr.tensor(std::vector<float>{ 9.0f });
      auto tm_b = mgr.tensor(std::vector<float>{ 4.0f });
      auto tm_c = mgr.tensor(std::vector<float>{ 0.0f });
      auto algo_min = mgr.algorithm({ tm_a, tm_b, tm_c }, vadd_spv, kp::Workgroup{ 1, 1, 1 });
      auto s = mgr.sequence();
      s->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ tm_a, tm_b, tm_c });
      s->eval<kp::OpAlgoDispatch>(algo_min);
      s->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tm_c });
      ok(feq(tm_c->vector()[0], 13.0f), "minimal 1-element workgroup(1,1,1) dispatch computes 9+4=13"); }

    // --- Sequence.clear(): drops recorded ops, leaves the sequence reusable ------------------------
    { auto tcl = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_cl = mgr.algorithm({ ta, tb, tcl }, vadd_spv, wg(N));
      auto scl = mgr.sequence();
      scl->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tcl });
      scl->eval<kp::OpAlgoDispatch>(algo_cl);
      scl->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tcl });
      ok(veq(tcl->vector(), ref_add), "pre-clear sequence computed a+b");
      scl->clear();
      ok(!scl->isRecording(), "sequence is not recording after clear()");
      ok(!scl->isRunning(), "sequence is not running after clear()");
      auto tcl2 = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_cl2 = mgr.algorithm({ ta, tb, tcl2 }, mul_spv, wg(N)); // different op -> a*b
      scl->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tcl2 });
      scl->eval<kp::OpAlgoDispatch>(algo_cl2);
      scl->eval<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tcl2 });
      ok(veq(tcl2->vector(), ref_mul) && !veq(tcl2->vector(), ref_add),
         "cleared sequence re-records a fresh op (a*b), not the pre-clear a+b batch"); }

    // --- Sequence.rerecord(): re-emits the same saved ops; deterministic re-execution --------------
    { auto tre = mgr.tensor(std::vector<float>(N, 0.0f));
      auto algo_re = mgr.algorithm({ ta, tb, tre }, saxpy_spv, wg(N),
                                   std::vector<float>{}, std::vector<float>{ 2.5f });
      auto sre = mgr.sequence();
      sre->record<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta, tb, tre });
      sre->record<kp::OpAlgoDispatch>(algo_re);
      sre->record<kp::OpTensorSyncLocal>(std::vector<std::shared_ptr<kp::Tensor>>{ tre });
      sre->eval();
      auto re_first = tre->vector();
      ok(veq(re_first, ref_saxpy(2.5f)), "pre-rerecord saxpy(2.5) computed 2.5a+b");
      sre->rerecord();
      sre->eval();
      ok(veq(tre->vector(), re_first),
         "rerecord()+eval reproduces the saved ops bit-for-bit (deterministic re-execution)"); }

    // --- destroy() lifecycle: an initialized resource reports not-init after destroy --------------
    { auto td_gone = mgr.tensor(std::vector<float>(4, 0.0f));
      ok(td_gone->isInit(), "tensor init before destroy");
      td_gone->destroy();
      ok(!td_gone->isInit(), "tensor reports not-init after destroy()"); }
    { auto algo_kill = mgr.algorithm({ ta, tb, tc }, vadd_spv, wg(N));
      ok(algo_kill->isInit(), "algorithm init before destroy");
      algo_kill->destroy();
      ok(!algo_kill->isInit(), "algorithm reports not-init after destroy()"); }
    { auto seq_kill = mgr.sequence();
      seq_kill->eval<kp::OpTensorSyncDevice>(std::vector<std::shared_ptr<kp::Tensor>>{ ta });
      ok(seq_kill->isInit(), "sequence init before destroy");
      seq_kill->destroy();
      ok(!seq_kill->isInit(), "sequence reports not-init after destroy()"); }

    // Manager.destroy(): tears down the device and every tensor it still manages -> managed tensor
    // reports not-init. Run on a throwaway manager so the primary mgr keeps serving the summary path.
    { kp::Manager mgr_kill;
      auto tk = mgr_kill.tensor(std::vector<float>(4, 0.0f));
      ok(tk->isInit(), "manager-owned tensor init before manager destroy");
      mgr_kill.destroy();
      ok(!tk->isInit(), "manager.destroy() releases its managed tensor (not-init)"); }

    int EXPECTED = 69, TOTAL = PASS + FAIL;
    printf("kompute-cpp: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d\n", PASS, FAIL, TOTAL, EXPECTED);
    if (FAIL == 0 && TOTAL == EXPECTED) { printf("KOMPUTE_CPP_FULL_API OK %d\n", PASS); return 0; }
    printf("KOMPUTE_CPP_FULL_API FAIL\n"); return 1;
}
