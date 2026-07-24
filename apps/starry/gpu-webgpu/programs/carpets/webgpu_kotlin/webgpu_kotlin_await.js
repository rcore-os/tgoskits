'use strict';
// Native async trampoline used by the Kotlin/JS carpet's `awaitDyn` suspend bridge.
//
// The dawn-based `webgpu` addon settles its promises from inside native event-processing callbacks.
// If the Kotlin continuation is resumed straight from a raw `promise.then(cb)` callback, `cb` runs
// while Dawn is still on its native callback frame, and the Kotlin code it resumes immediately calls
// back into Dawn (createCommandEncoder / submit / destroy) - re-entering the addon mid-callback and
// segfaulting. Awaiting the promise inside a real JS `async` function instead defers the resume onto
// V8's genuine promise-job queue, which runs only after the native frame has fully unwound - exactly
// how plain `await` behaves. This file is plain JS because the Kotlin `js()` intrinsic rejects
// async/await literals.
module.exports.awaitVia = function awaitVia(promise, onOk, onErr) {
  (async () => {
    try {
      onOk(await promise);
    } catch (e) {
      onErr(e);
    }
  })();
};
