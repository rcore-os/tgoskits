# webgpu_kotlin host wall: Kotlin/JS coroutine x Dawn native re-entrancy

## Environment
- Host: Mesa 25.2.8 lavapipe (LLVM 20.1.2), webgpu npm 0.4.0 (dawn addon), Node v20.19.5,
  kotlinc-js 2.4.0 (JRE 17), LP_NUM_THREADS=1.

## Symptom
The full webgpu_kotlin carpet compiles cleanly (Kotlin/JS IR: .kt -> .klib -> commonjs .js) and
runs up to adapter+device setup, then crashes non-deterministically (SIGSEGV / glibc pthread_mutex
assertion / std::system_error / futex error / SIGFPE) inside the dawn addon.

## Isolation (minimal reproducers under this dir)
1. LP_NUM_THREADS=1 fixes the plain JS/TS carpets (webgpu_js 78/78, 5/5 runs) - llvmpipe's
   multi-threaded rasterizer was racing dawn's callback threads. Kept in the harness.
2. With LP_NUM_THREADS=1 the Kotlin carpet STILL crashes. Checkpoint markers pin the first crash to
   the first `awaitDyn(...)` that runs AFTER a compute pipeline exists (popErrorScope OR mapAsync):
   repro_kotlin_popErrorScope_CRASHES.kt -> 5/5 SIGSEGV right after "pipe".
3. Control: the identical operation sequence AND the identical `awaitVia` async-trampoline expressed
   as pure-JS manual CPS callbacks (control_purejs_cps_trampoline_STABLE.js) is 5/5 CLEAN. So the
   trampoline JS and the op sequence are not the fault.
4. Control: `awaitDyn(Promise.resolve(42))` AFTER pipeline creation in Kotlin is 5/5 CLEAN
   (repro_kotlin_trivialPromise_after_pipeline_STABLE.kt). So Kotlin coroutine resume per se and the
   existence of dawn worker threads are not the fault - only awaiting a *dawn-native* promise
   (popErrorScope/mapAsync) after a pipeline exists crashes.
5. Five trampoline deferral strategies tried and all crash: bare async/await (STABLE variant),
   setImmediate-after-await, double-microtask, Promise.then-chain, setTimeout(0), and deferring the
   Kotlin `cont.resume` itself via setImmediate. Deferring made it crash earlier, not later - so it
   is not a resume-timing problem.

## Root cause (as far as isolated)
The Kotlin/JS 2.4.0 coroutine state-machine continuation, when it resumes across a suspend point that
awaited a dawn-native promise and then re-enters the dawn addon (createCommandEncoder/submit/
popErrorScope) while a compute pipeline is live, corrupts dawn's native state. The identical control
flow in pure JS (async/await OR manual CPS with the same trampoline) does not. This is a
Kotlin/JS-runtime x dawn(webgpu@0.4.0) native-interop wall on this host, independent of the carpet
logic (which is complete, 78 pinned assertions).

## Status
webgpu_kotlin cell SOURCE is complete and compiles; it is documented as CI/on-target only for the
host layer (the host dawn+Kotlin/JS combo is the wall). The JS/TS webgpu cells and wgpu C/C++ cells
run host-green with LP_NUM_THREADS=1.
