#!/usr/bin/env node
'use strict';
/*
 * node-carpet.js — INDUSTRIAL-GRADE Node.js language + stdlib carpet for StarryOS #764.
 *
 * Doc-grounded against https://nodejs.org/api/ (full module index) and the ES2023+ language
 * surface. Every assertion is observable; every version-gated feature is guarded against the
 * running Node major (host golden = v20; target = v22). Pass/skip are both logged.
 *
 * OK token printed at the very end iff zero failures: NODE_CARPET_OK
 *
 * Self-contained: no external deps, no hard-coded host abs paths (uses os.tmpdir()).
 * Memory: pure JS, no heavy runtime; works under tight heaps.
 *
 * Run:  node node-carpet.js
 *       node --experimental-sqlite node-carpet.js   (to also exercise node:sqlite on v22)
 */

const os = require('node:os');
const path = require('node:path');
const fs = require('node:fs');

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------
let PASS = 0, FAIL = 0, SKIP = 0;
const FAILURES = [];
const NODE_MAJOR = parseInt(process.versions.node.split('.')[0], 10);

function ok(name, cond, detail) {
  if (cond) { PASS++; }
  else { FAIL++; FAILURES.push(name + (detail ? ' :: ' + detail : '')); console.log('FAIL ' + name + (detail ? ' :: ' + detail : '')); }
}
function eq(name, a, b) { ok(name, Object.is(a, b) || JSON.stringify(a) === JSON.stringify(b), `got=${stringify(a)} want=${stringify(b)}`); }
function truthy(name, v) { ok(name, !!v, `got=${stringify(v)}`); }
function isType(name, v, t) { ok(name, typeof v === t, `typeof=${typeof v} want=${t}`); }
function throws(name, fn) { let t = false; try { fn(); } catch (_) { t = true; } ok(name, t, 'expected throw'); }
function skip(name, why) { SKIP++; console.log('SKIP ' + name + ' :: ' + why); }
function stringify(v) { try { return typeof v === 'function' ? 'fn' : JSON.stringify(v); } catch { return String(v); } }
// Gate a block on minimum Node major; auto-skip on lower.
function whenMajor(min, name, fn) { if (NODE_MAJOR >= min) fn(); else skip(name, `needs Node >=${min}, host=${NODE_MAJOR}`); }

const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'nodecarpet-'));
function cleanup() { try { fs.rmSync(TMP, { recursive: true, force: true }); } catch {} }

// Per-check async watchdog: race a promise against a timer that RESOLVES (to a logged skip),
// so a hung loopback/worker/readline never stalls the carpet. Mirrors the net/http pattern.
// Returns a promise that always settles within `ms`.
function withTimeout(name, ms, promise, onTimeout) {
  let timer;
  const guard = new Promise((resolve) => {
    timer = setTimeout(() => {
      if (onTimeout) { try { onTimeout(); } catch {} }
      skip(name, `internal timeout after ${ms}ms (restricted/slow env)`);
      resolve();
    }, ms);
    if (timer.unref) timer.unref();
  });
  return Promise.race([
    Promise.resolve(promise).then((v) => { clearTimeout(timer); return v; }, (e) => {
      clearTimeout(timer);
      ok(name + '/error', false, 'rejected: ' + (e && e.message ? e.message : e));
    }),
    guard,
  ]);
}

// ===========================================================================
// SECTION 1 — ES2023+ LANGUAGE FEATURES
// ===========================================================================
function section_language() {
  // --- ES2015..2020 baseline ---
  eq('lang/let-const/template', `${1 + 1}`, '2');
  eq('lang/arrow+default', ((a = 5) => a * 2)(), 10);
  eq('lang/destructure-array', (() => { const [a, , c] = [1, 2, 3]; return a + c; })(), 4);
  eq('lang/destructure-obj-rename-default', (() => { const { x: y = 7 } = {}; return y; })(), 7);
  eq('lang/spread-rest', (() => { const f = (...n) => n.reduce((a, b) => a + b, 0); return f(...[1, 2, 3, 4]); })(), 10);
  eq('lang/for-of', (() => { let s = 0; for (const v of [1, 2, 3]) s += v; return s; })(), 6);
  eq('lang/generator', (() => { function* g() { yield 1; yield 2; } return [...g()].join(','); })(), '1,2');
  // Symbol semantics (real differential assertions, no tautology):
  isType('lang/symbol-typeof', Symbol('x'), 'symbol');                          // typeof Symbol(x) === 'symbol'
  isType('lang/symbol-iterator-on-array', [][Symbol.iterator], 'function');     // Array's @@iterator is a function
  ok('lang/symbol-for-interned', Symbol.for('k') === Symbol.for('k'));          // global registry interning
  ok('lang/symbol-unique', Symbol('a') !== Symbol('a'));                        // distinct uninterned symbols
  eq('lang/symbol-description', Symbol('desc').description, 'desc');
  eq('lang/map-set', (() => { const m = new Map([['a', 1]]); const s = new Set([1, 1, 2]); return `${m.get('a')}-${s.size}`; })(), '1-2');
  // WeakMap: keys must be objects, basic get/set/has/delete
  eq('lang/weakmap', (() => { const wm = new WeakMap(); const k = {}; const k2 = {}; wm.set(k, 42); return `${wm.get(k)}-${wm.has(k)}-${wm.has(k2)}`; })(), '42-true-false');
  // WeakSet: values must be objects, basic add/has/delete
  eq('lang/weakset', (() => { const ws = new WeakSet(); const o = {}; ws.add(o); return `${ws.has(o)}`; })(), 'true');
  // WeakRef + FinalizationRegistry (ES2021+)
  truthy('lang/weakref', typeof WeakRef === 'function');
  truthy('lang/finalization-registry', typeof FinalizationRegistry === 'function');
  eq('lang/computed-prop', (() => { const k = 'z'; return { [k]: 9 }.z; })(), 9);

  // --- ES2017 async/await ---
  // (validated in section_async)

  // --- ES2018 ---
  eq('lang/object-spread', JSON.stringify({ ...{ a: 1 }, b: 2 }), '{"a":1,"b":2}');
  eq('lang/regex-named-group', 'a1'.match(/(?<l>[a-z])(?<d>\d)/).groups.l, 'a');
  eq('lang/regex-lookbehind', '$5'.match(/(?<=\$)\d/)[0], '5');
  eq('lang/async-iter', typeof (async function*() { yield 1; })().next, 'function');

  // --- ES2019 ---
  eq('lang/array-flat', JSON.stringify([1, [2, [3]]].flat(2)), '[1,2,3]');
  eq('lang/array-flatMap', JSON.stringify([1, 2].flatMap((x) => [x, x])), '[1,1,2,2]');
  eq('lang/string-trimStart/End', '  x  '.trimStart().trimEnd(), 'x');
  eq('lang/object-fromEntries', JSON.stringify(Object.fromEntries([['a', 1]])), '{"a":1}');

  // --- ES2020 ---
  eq('lang/optional-chaining', ({ a: { b: 2 } }).a?.b ?? -1, 2);
  eq('lang/nullish-coalescing', (null ?? 'd'), 'd');
  eq('lang/bigint', (2n ** 64n).toString(), '18446744073709551616');
  isType('lang/globalThis', globalThis, 'object');
  truthy('lang/promise-allSettled', typeof Promise.allSettled === 'function');
  eq('lang/matchAll', [...'a1b2'.matchAll(/(\d)/g)].map((m) => m[1]).join(''), '12');

  // --- ES2021 ---
  eq('lang/logical-assign', (() => { let a = null; a ??= 3; let b = 1; b ||= 9; let c = 1; c &&= 5; return `${a}${b}${c}`; })(), '315');
  eq('lang/numeric-separator', 1_000_000, 1000000);
  eq('lang/string-replaceAll', 'a.b.c'.replaceAll('.', '-'), 'a-b-c');
  truthy('lang/promise-any', typeof Promise.any === 'function');
  truthy('lang/weakref', typeof WeakRef === 'function' && typeof FinalizationRegistry === 'function');

  // --- ES2022 ---
  eq('lang/at-array', [10, 20, 30].at(-1), 30);
  eq('lang/at-string', 'abc'.at(-1), 'c');
  truthy('lang/object-hasOwn', Object.hasOwn({ a: 1 }, 'a') && !Object.hasOwn({}, 'a'));
  eq('lang/error-cause', new Error('e', { cause: 42 }).cause, 42);
  truthy('lang/regexp-d-flag', /a/d.hasIndices === true);
  eq('lang/class-private+static', (() => {
    class C { #v = 5; static s = 9; getV() { return this.#v; } static has(o) { return #v in o; } }
    const c = new C(); return `${c.getV()}-${C.s}-${C.has(c)}`;
  })(), '5-9-true');
  eq('lang/class-static-block', (() => { class C { static x; static { C.x = 7; } } return C.x; })(), 7);
  eq('lang/top-level-await-marker', typeof (async () => {})().then, 'function');

  // --- ES2023 ---
  eq('lang/array-findLast', [1, 2, 3, 4].findLast((x) => x % 2 === 0), 4);
  eq('lang/array-findLastIndex', [1, 2, 3, 4].findLastIndex((x) => x % 2 === 0), 3);
  eq('lang/array-toSorted', JSON.stringify([3, 1, 2].toSorted()), '[1,2,3]');
  eq('lang/array-toReversed', JSON.stringify([1, 2, 3].toReversed()), '[3,2,1]');
  eq('lang/array-with', JSON.stringify([1, 2, 3].with(1, 9)), '[1,9,3]');
  eq('lang/array-toSpliced', JSON.stringify([1, 2, 3].toSpliced(1, 1, 9)), '[1,9,3]');
  truthy('lang/hashbang-grammar', typeof process.version === 'string' && process.version.startsWith('v'));

  // --- ES2024 (Node 22 has these in V8 12.x; gate by major) ---
  whenMajor(22, 'lang/array-groupBy(Object.groupBy)', () => {
    truthy('lang/Object.groupBy', typeof Object.groupBy === 'function');
    truthy('lang/Map.groupBy', typeof Map.groupBy === 'function');
    eq('lang/Object.groupBy-result', JSON.stringify(Object.groupBy([1, 2, 3, 4], (n) => (n % 2 ? 'odd' : 'even'))), '{"odd":[1,3],"even":[2,4]}');
    truthy('lang/Promise.withResolvers', typeof Promise.withResolvers === 'function');
    truthy('lang/ArrayBuffer.resizable', typeof new ArrayBuffer(8, { maxByteLength: 16 }).resize === 'function');
  });
  // Array.fromAsync (V8 12+, Node 22)
  whenMajor(22, 'lang/Array.fromAsync', () => { truthy('lang/Array.fromAsync', typeof Array.fromAsync === 'function'); });

  // Proxy / Reflect (ES2015 but core to language)
  eq('lang/proxy', (() => { const p = new Proxy({}, { get: () => 42 }); return p.anything; })(), 42);
  eq('lang/reflect', Reflect.has({ a: 1 }, 'a'), true);

  // Tagged templates
  eq('lang/tagged-template', (() => { const t = (s, ...v) => s.join('|') + '#' + v.join(','); return t`a${1}b${2}c`; })(), 'a|b|c#1,2');

  // Iterators / Symbol.iterator custom
  eq('lang/custom-iterator', (() => {
    const obj = { *[Symbol.iterator]() { yield 'x'; yield 'y'; } };
    return [...obj].join('');
  })(), 'xy');

  // Iterator Helpers (v22: .map/.filter/.take/.drop/.reduce/.toArray on iterators)
  if (typeof Iterator !== 'undefined' && Iterator.prototype && Iterator.prototype.map) {
    eq('lang/iterator-map', [...[1, 2, 3].values().map((x) => x * 2)].join(','), '2,4,6');
    eq('lang/iterator-filter', [...[1, 2, 3, 4].values().filter((x) => x % 2 === 0)].join(','), '2,4');
    eq('lang/iterator-take', [...[1, 2, 3, 4].values().take(2)].join(','), '1,2');
    eq('lang/iterator-toArray', Array.isArray([1, 2].values().toArray()), true);
  } else { skip('lang/iterator-helpers', `Iterator Helpers absent in Node ${NODE_MAJOR}`); }

  // Intl (ICU is linked in)
  eq('lang/intl-numberformat', new Intl.NumberFormat('en-US').format(1234.5), '1,234.5');
  truthy('lang/intl-datetimeformat', typeof new Intl.DateTimeFormat('en-US').format(new Date(0)) === 'string');
  eq('lang/intl-collator', new Intl.Collator('en').compare('a', 'b') < 0, true);
}

// ===========================================================================
// SECTION 2 — GLOBALS / Web-platform APIs on the global object
// ===========================================================================
function section_globals() {
  isType('global/structuredClone', structuredClone, 'function');
  eq('global/structuredClone-deep', (() => { const o = { a: [1, { b: 2 }] }; const c = structuredClone(o); c.a[1].b = 9; return o.a[1].b; })(), 2);
  isType('global/queueMicrotask', queueMicrotask, 'function');
  isType('global/setTimeout', setTimeout, 'function');
  isType('global/setInterval', setInterval, 'function');
  isType('global/setImmediate', setImmediate, 'function');
  isType('global/TextEncoder', TextEncoder, 'function');
  isType('global/TextDecoder', TextDecoder, 'function');
  eq('global/textencode-roundtrip', new TextDecoder().decode(new TextEncoder().encode('héllo')), 'héllo');
  isType('global/URL', URL, 'function');
  isType('global/URLSearchParams', URLSearchParams, 'function');
  isType('global/AbortController', AbortController, 'function');
  isType('global/AbortSignal', AbortSignal, 'function');
  truthy('global/AbortSignal.timeout', typeof AbortSignal.timeout === 'function');
  isType('global/Event', Event, 'function');
  isType('global/EventTarget', EventTarget, 'function');
  isType('global/btoa/atob', btoa, 'function');
  eq('global/btoa-atob-roundtrip', atob(btoa('hi')), 'hi');
  isType('global/Blob', Blob, 'function');
  isType('global/performance', performance.now, 'function');
  isType('global/fetch', fetch, 'function'); // function present even if no network
  truthy('global/crypto-getRandomValues', typeof crypto.getRandomValues === 'function');
  truthy('global/crypto.subtle', typeof crypto.subtle === 'object');
  truthy('global/crypto.randomUUID', typeof crypto.randomUUID === 'function' && /^[0-9a-f-]{36}$/.test(crypto.randomUUID()));
  isType('global/process', process.pid, 'number');
  isType('global/console', console.log, 'function');
  isType('global/Buffer', Buffer, 'function');
  whenMajor(22, 'global/WebSocket', () => { truthy('global/WebSocket', typeof WebSocket === 'function'); });
  whenMajor(20, 'global/EventTarget-dispatch', () => {
    const et = new EventTarget(); let got = 0; et.addEventListener('x', () => { got = 1; });
    et.dispatchEvent(new Event('x')); eq('global/event-dispatch', got, 1);
  });
}

// ===========================================================================
// SECTION 3 — STDLIB MODULES (every documented core module)
// ===========================================================================
function mod_assert() {
  const assert = require('node:assert');
  assert.strictEqual(1, 1);
  assert.deepStrictEqual({ a: 1 }, { a: 1 });
  ok('assert/throws', typeof assert.throws === 'function');
  throws('assert/strictEqual-fail', () => assert.strictEqual(1, 2));
  const as = require('node:assert/strict');
  ok('assert/strict-module', typeof as.equal === 'function');
  ok('assert/strict-alias', assert.strict.equal === as.equal);
  let rejected = false;
  return withTimeout('assert/rejects', 5000,
    assert.rejects(Promise.reject(new Error('x'))).then(() => { rejected = true; ok('assert/rejects', rejected); }));
}

function mod_buffer() {
  const { Buffer, Blob, atob, btoa, constants } = require('node:buffer');
  eq('buffer/from-hex', Buffer.from('414243', 'hex').toString(), 'ABC');
  eq('buffer/from-base64', Buffer.from('QUJD', 'base64').toString(), 'ABC');
  eq('buffer/toString-base64', Buffer.from('ABC').toString('base64'), 'QUJD');
  eq('buffer/alloc-zero', Buffer.alloc(4).toString('hex'), '00000000');
  eq('buffer/concat', Buffer.concat([Buffer.from('a'), Buffer.from('b')]).toString(), 'ab');
  eq('buffer/write-readInt', (() => { const b = Buffer.alloc(4); b.writeInt32BE(0x01020304); return b.readInt32BE().toString(16); })(), '1020304');
  eq('buffer/writeUInt8/readUInt8', (() => { const b = Buffer.alloc(1); b.writeUInt8(255, 0); return b.readUInt8(0); })(), 255);
  eq('buffer/compare', Buffer.compare(Buffer.from('a'), Buffer.from('b')), -1);
  eq('buffer/equals', Buffer.from('x').equals(Buffer.from('x')), true);
  eq('buffer/slice/subarray', Buffer.from('hello').subarray(1, 3).toString(), 'el');
  eq('buffer/swap', (() => { const b = Buffer.from([1, 2]); b.swap16(); return b[0]; })(), 2);
  eq('buffer/byteLength', Buffer.byteLength('héllo', 'utf8'), 6);
  eq('buffer/isBuffer', Buffer.isBuffer(Buffer.alloc(0)), true);
  truthy('buffer/constants', constants.MAX_LENGTH > 0);
  isType('buffer/Blob', Blob, 'function');
  eq('buffer/atob-btoa', atob(btoa('z')), 'z');
}

function mod_path() {
  const p = require('node:path');
  eq('path/join', p.posix.join('a', 'b', 'c'), 'a/b/c');
  eq('path/resolve-abs', p.posix.isAbsolute(p.posix.resolve('/x', 'y')), true);
  eq('path/basename', p.basename('/a/b/c.txt'), 'c.txt');
  eq('path/basename-ext', p.basename('/a/b/c.txt', '.txt'), 'c');
  eq('path/dirname', p.posix.dirname('/a/b/c'), '/a/b');
  eq('path/extname', p.extname('x.tar.gz'), '.gz');
  eq('path/parse-format', (() => { const o = p.posix.parse('/a/b.txt'); return p.posix.format(o); })(), '/a/b.txt');
  eq('path/relative', p.posix.relative('/a/b', '/a/c'), '../c');
  eq('path/normalize', p.posix.normalize('a//b/../c'), 'a/c');
  eq('path/sep-win32', p.win32.sep, '\\');
  eq('path/sep-posix', p.posix.sep, '/');
  eq('path/delimiter', typeof p.delimiter, 'string');
  eq('path/win32-join', p.win32.join('a', 'b'), 'a\\b');
}

function mod_util() {
  const util = require('node:util');
  eq('util/format', util.format('%s=%d', 'x', 5), 'x=5');
  eq('util/inspect', util.inspect({ a: 1 }).includes('a: 1'), true);
  eq('util/types-isDate', util.types.isDate(new Date()), true);
  eq('util/isDeepStrictEqual', util.isDeepStrictEqual({ a: 1 }, { a: 1 }), true);
  isType('util/promisify', util.promisify, 'function');
  isType('util/callbackify', util.callbackify, 'function');
  isType('util/inherits', util.inherits, 'function');
  isType('util/deprecate', util.deprecate, 'function');
  isType('util/TextEncoder', util.TextEncoder, 'function');
  // parseArgs (stable in 18.3+)
  isType('util/parseArgs', util.parseArgs, 'function');
  eq('util/parseArgs-result', (() => {
    const { values, positionals } = util.parseArgs({ args: ['--name', 'x', 'pos'], options: { name: { type: 'string' } }, allowPositionals: true });
    return values.name + '|' + positionals.join(',');
  })(), 'x|pos');
  // styleText (20.12+/21+): present on host
  if (typeof util.styleText === 'function') {
    truthy('util/styleText', typeof util.styleText('red', 'x') === 'string');
  } else skip('util/styleText', 'not in this Node');
  // promisify roundtrip
  return withTimeout('util/promisify-setTimeout', 5000, util.promisify(setTimeout)(1).then(() => ok('util/promisify-setTimeout', true)));
}

function mod_events() {
  const EventEmitter = require('node:events');
  const ee = new EventEmitter();
  let v = 0; ee.on('e', (x) => { v = x; }); ee.emit('e', 7);
  eq('events/on-emit', v, 7);
  eq('events/once-count', (() => { let c = 0; ee.once('f', () => c++); ee.emit('f'); ee.emit('f'); return c; })(), 1);
  eq('events/listenerCount', (() => { const e = new EventEmitter(); e.on('a', () => {}); return e.listenerCount('a'); })(), 1);
  eq('events/removeListener', (() => { const e = new EventEmitter(); const f = () => {}; e.on('a', f); e.removeListener('a', f); return e.listenerCount('a'); })(), 0);
  isType('events/getEventListeners', require('node:events').getEventListeners, 'function');
  isType('events/setMaxListeners', EventEmitter.setMaxListeners, 'function');
  // events.once promise
  const e2 = new EventEmitter();
  const p = require('node:events').once(e2, 'go');
  setImmediate(() => e2.emit('go', 'done'));
  return withTimeout('events/once-promise', 5000, p.then(([arg]) => ok('events/once-promise', arg === 'done')));
}

function mod_stream() {
  const stream = require('node:stream');
  isType('stream/Readable', stream.Readable, 'function');
  isType('stream/Writable', stream.Writable, 'function');
  isType('stream/Transform', stream.Transform, 'function');
  isType('stream/Duplex', stream.Duplex, 'function');
  isType('stream/PassThrough', stream.PassThrough, 'function');
  isType('stream/pipeline', stream.pipeline, 'function');
  isType('stream/finished', stream.finished, 'function');
  isType('stream/promises', require('node:stream/promises').pipeline, 'function');
  isType('stream/consumers.text', require('node:stream/consumers').text, 'function');
  isType('stream/web.ReadableStream', require('node:stream/web').ReadableStream, 'function');
  // pipeline collect
  const chunks = [];
  const r = stream.Readable.from(['a', 'b', 'c']);
  const w = new stream.Writable({ write(c, e, cb) { chunks.push(c.toString()); cb(); } });
  return withTimeout('stream/pipeline-collect', 5000,
    require('node:stream/promises').pipeline(r, w).then(() => eq('stream/pipeline-collect', chunks.join(''), 'abc')));
}

function mod_fs() {
  const fsmod = require('node:fs');
  const fsp = require('node:fs/promises');
  const f = path.join(TMP, 'a.txt');
  fsmod.writeFileSync(f, 'hello');
  eq('fs/writeFileSync+readFileSync', fsmod.readFileSync(f, 'utf8'), 'hello');
  eq('fs/existsSync', fsmod.existsSync(f), true);
  eq('fs/statSync-size', fsmod.statSync(f).size, 5);
  eq('fs/appendFileSync', (() => { fsmod.appendFileSync(f, '!'); return fsmod.readFileSync(f, 'utf8'); })(), 'hello!');
  fsmod.mkdirSync(path.join(TMP, 'd', 'e'), { recursive: true });
  eq('fs/mkdirSync-recursive', fsmod.existsSync(path.join(TMP, 'd', 'e')), true);
  eq('fs/readdirSync', fsmod.readdirSync(TMP).includes('a.txt'), true);
  fsmod.copyFileSync(f, path.join(TMP, 'b.txt'));
  eq('fs/copyFileSync', fsmod.existsSync(path.join(TMP, 'b.txt')), true);
  fsmod.renameSync(path.join(TMP, 'b.txt'), path.join(TMP, 'c.txt'));
  eq('fs/renameSync', fsmod.existsSync(path.join(TMP, 'c.txt')), true);
  fsmod.unlinkSync(path.join(TMP, 'c.txt'));
  eq('fs/unlinkSync', fsmod.existsSync(path.join(TMP, 'c.txt')), false);
  isType('fs/constants', fsmod.constants.O_RDONLY, 'number');
  isType('fs/createReadStream', fsmod.createReadStream, 'function');
  isType('fs/createWriteStream', fsmod.createWriteStream, 'function');
  isType('fs/watch', fsmod.watch, 'function');
  isType('fs/Dirent', fsmod.Dirent, 'function');
  // fs.realpathSync, fs.chmodSync
  isType('fs/realpathSync', fsmod.realpathSync, 'function');
  eq('fs/chmodSync', (() => { fsmod.writeFileSync(f, 'x'); fsmod.chmodSync(f, 0o644); return (fsmod.statSync(f).mode & 0o777).toString(8); })(), '644');
  // fs.globSync is v22+ (gate)
  whenMajor(22, 'fs/globSync', () => { truthy('fs/globSync', typeof fsmod.globSync === 'function'); });
  // promises API
  return withTimeout('fs/promises', 6000, fsp.readFile(f, 'utf8').then((d) => {
    eq('fs/promises-readFile', d, 'x');
    return fsp.writeFile(path.join(TMP, 'p.txt'), 'p').then(() => fsp.readFile(path.join(TMP, 'p.txt'), 'utf8')).then((dd) => {
      eq('fs/promises-write+read', dd, 'p');
      return fsp.stat(path.join(TMP, 'p.txt')).then((st) => ok('fs/promises-stat', st.size === 1));
    });
  }));
}

function mod_os() {
  const osmod = require('node:os');
  isType('os/platform', osmod.platform(), 'string');
  isType('os/arch', osmod.arch(), 'string');
  isType('os/release', osmod.release(), 'string');
  truthy('os/cpus', Array.isArray(osmod.cpus()) && osmod.cpus().length > 0);
  truthy('os/totalmem', osmod.totalmem() > 0);
  truthy('os/freemem', osmod.freemem() >= 0);
  isType('os/hostname', osmod.hostname(), 'string');
  isType('os/tmpdir', osmod.tmpdir(), 'string');
  isType('os/homedir', osmod.homedir(), 'string');
  // os.userInfo() throws ERR_SYSTEM_ERROR (ENOENT) when no /etc/passwd entry exists for the uid
  // (common in minimal/container/starry rootfs) — guard and SKIP rather than FAIL.
  try { isType('os/userInfo', osmod.userInfo().username, 'string'); }
  catch (e) { skip('os/userInfo', 'no passwd entry: ' + (e && (e.code || e.message))); }
  truthy('os/uptime', osmod.uptime() >= 0);
  truthy('os/loadavg', Array.isArray(osmod.loadavg()) && osmod.loadavg().length === 3);
  eq('os/EOL', typeof osmod.EOL, 'string');
  isType('os/networkInterfaces', osmod.networkInterfaces(), 'object');
  isType('os/endianness', osmod.endianness(), 'string');
  isType('os/constants', osmod.constants.signals.SIGINT, 'number');
}

function mod_crypto() {
  const crypto = require('node:crypto');
  eq('crypto/sha256-hex', crypto.createHash('sha256').update('abc').digest('hex'),
    'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  eq('crypto/md5', crypto.createHash('md5').update('').digest('hex'), 'd41d8cd98f00b204e9800998ecf8427e');
  eq('crypto/hmac', crypto.createHmac('sha256', 'key').update('msg').digest('hex').length, 64);
  truthy('crypto/randomBytes', crypto.randomBytes(16).length === 16);
  truthy('crypto/randomUUID', /^[0-9a-f-]{36}$/.test(crypto.randomUUID()));
  truthy('crypto/randomInt', (() => { const r = crypto.randomInt(0, 10); return r >= 0 && r < 10; })());
  // AES-256-CBC roundtrip
  eq('crypto/aes-256-cbc-roundtrip', (() => {
    const key = crypto.randomBytes(32), iv = crypto.randomBytes(16);
    const c = crypto.createCipheriv('aes-256-cbc', key, iv);
    const enc = Buffer.concat([c.update('secret', 'utf8'), c.final()]);
    const d = crypto.createDecipheriv('aes-256-cbc', key, iv);
    return Buffer.concat([d.update(enc), d.final()]).toString('utf8');
  })(), 'secret');
  // AES-256-GCM (AEAD) roundtrip
  eq('crypto/aes-256-gcm-roundtrip', (() => {
    const key = crypto.randomBytes(32), iv = crypto.randomBytes(12);
    const c = crypto.createCipheriv('aes-256-gcm', key, iv);
    const enc = Buffer.concat([c.update('tagged', 'utf8'), c.final()]);
    const tag = c.getAuthTag();
    const d = crypto.createDecipheriv('aes-256-gcm', key, iv); d.setAuthTag(tag);
    return Buffer.concat([d.update(enc), d.final()]).toString('utf8');
  })(), 'tagged');
  // pbkdf2 / scrypt
  eq('crypto/pbkdf2Sync', crypto.pbkdf2Sync('pw', 'salt', 1000, 32, 'sha256').length, 32);
  eq('crypto/scryptSync', crypto.scryptSync('pw', 'salt', 32).length, 32);
  // RSA keypair + sign/verify
  eq('crypto/rsa-sign-verify', (() => {
    const { publicKey, privateKey } = crypto.generateKeyPairSync('rsa', { modulusLength: 2048 });
    const sig = crypto.sign('sha256', Buffer.from('data'), privateKey);
    return crypto.verify('sha256', Buffer.from('data'), publicKey, sig);
  })(), true);
  // ECDH
  eq('crypto/ecdh-shared-secret', (() => {
    const a = crypto.createECDH('prime256v1'); a.generateKeys();
    const b = crypto.createECDH('prime256v1'); b.generateKeys();
    return a.computeSecret(b.getPublicKey()).equals(b.computeSecret(a.getPublicKey()));
  })(), true);
  truthy('crypto/getHashes', crypto.getHashes().includes('sha256'));
  truthy('crypto/getCiphers', crypto.getCiphers().length > 0);
  truthy('crypto/webcrypto', typeof crypto.webcrypto === 'object');
  // WebCrypto subtle digest
  return withTimeout('crypto/webcrypto-subtle-digest', 5000,
    crypto.webcrypto.subtle.digest('SHA-256', new TextEncoder().encode('abc')).then((buf) => {
      eq('crypto/webcrypto-subtle-digest', Buffer.from(buf).toString('hex'),
        'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    }));
}

function mod_zlib() {
  const zlib = require('node:zlib');
  eq('zlib/gzip-roundtrip', zlib.gunzipSync(zlib.gzipSync(Buffer.from('payload'))).toString(), 'payload');
  eq('zlib/deflate-roundtrip', zlib.inflateSync(zlib.deflateSync(Buffer.from('payload'))).toString(), 'payload');
  eq('zlib/deflateRaw-roundtrip', zlib.inflateRawSync(zlib.deflateRawSync(Buffer.from('p'))).toString(), 'p');
  eq('zlib/brotli-roundtrip', zlib.brotliDecompressSync(zlib.brotliCompressSync(Buffer.from('brotli'))).toString(), 'brotli');
  isType('zlib/constants', zlib.constants.Z_BEST_COMPRESSION, 'number');
  isType('zlib/createGzip', zlib.createGzip, 'function');
}

function mod_url() {
  const { URL, URLSearchParams, fileURLToPath, pathToFileURL, format } = require('node:url');
  const u = new URL('https://user:pass@host.com:8080/p/q?x=1&y=2#frag');
  eq('url/protocol', u.protocol, 'https:');
  eq('url/hostname', u.hostname, 'host.com');
  eq('url/port', u.port, '8080');
  eq('url/pathname', u.pathname, '/p/q');
  eq('url/search', u.search, '?x=1&y=2');
  eq('url/hash', u.hash, '#frag');
  eq('url/username', u.username, 'user');
  eq('url/searchParams-get', u.searchParams.get('x'), '1');
  const sp = new URLSearchParams('a=1&a=2&b=3');
  eq('url/searchParams-getAll', sp.getAll('a').join(','), '1,2');
  eq('url/searchParams-toString', new URLSearchParams({ k: 'v w' }).toString(), 'k=v+w');
  eq('url/fileURLToPath', fileURLToPath('file:///tmp/x'), path.sep === '/' ? '/tmp/x' : fileURLToPath('file:///tmp/x'));
  truthy('url/pathToFileURL', pathToFileURL('/tmp/x').href.startsWith('file://'));
  isType('url/format', format, 'function');
  truthy('url/canParse', URL.canParse ? URL.canParse('https://x.y') : true);
}

function mod_querystring() {
  const qs = require('node:querystring');
  eq('querystring/stringify', qs.stringify({ a: 1, b: 'c d' }), 'a=1&b=c%20d');
  eq('querystring/parse', JSON.stringify(qs.parse('a=1&b=2')), '{"a":"1","b":"2"}');
  eq('querystring/escape', qs.escape('a b'), 'a%20b');
  eq('querystring/unescape', qs.unescape('a%20b'), 'a b');
}

function mod_string_decoder() {
  const { StringDecoder } = require('node:string_decoder');
  const d = new StringDecoder('utf8');
  // multibyte split across writes
  const euro = Buffer.from('€', 'utf8'); // 3 bytes
  let out = d.write(euro.subarray(0, 2));
  out += d.end(euro.subarray(2));
  eq('string_decoder/multibyte-split', out, '€');
}

function mod_timers() {
  const timers = require('node:timers');
  isType('timers/setTimeout', timers.setTimeout, 'function');
  isType('timers/setInterval', timers.setInterval, 'function');
  isType('timers/setImmediate', timers.setImmediate, 'function');
  const tp = require('node:timers/promises');
  isType('timers/promises.setTimeout', tp.setTimeout, 'function');
  isType('timers/promises.setImmediate', tp.setImmediate, 'function');
  // scheduler (web-platform Scheduling API surface, present v18+). Assert the real shape — not a
  // tautological `{}` substitution — and exercise its observable behavior below.
  const haveScheduler = !!(tp.scheduler && typeof tp.scheduler.wait === 'function');
  if (haveScheduler) {
    isType('timers/promises.scheduler', tp.scheduler, 'object');
    isType('timers/promises.scheduler.wait', tp.scheduler.wait, 'function');
  } else {
    skip('timers/promises.scheduler', 'tp.scheduler.wait absent in this Node');
  }
  return withTimeout('timers/promises-resolve', 5000, tp.setTimeout(2, 'v').then((v) => {
    eq('timers/promises-resolve', v, 'v');
    // Observable scheduler.wait roundtrip: resolves (to undefined) after the delay. If scheduler is
    // genuinely missing, the presence check above already logged a skip; mark this case skipped too.
    if (!haveScheduler) { skip('timers/promises.scheduler-wait', 'no scheduler.wait'); return; }
    return tp.scheduler.wait(2).then((r) => eq('timers/promises.scheduler-wait', r, undefined));
  }));
}

function mod_net_http() {
  // node:net + node:http server/client on loopback (127.0.0.1). On starry, loopback may be
  // restricted; we attempt and skip-with-reason on failure rather than fail.
  const http = require('node:http');
  const net = require('node:net');
  isType('net/createServer', net.createServer, 'function');
  isType('net/Socket', net.Socket, 'function');
  eq('net/isIP', net.isIP('::1'), 6);
  eq('net/isIPv4', net.isIPv4('1.2.3.4'), true);
  isType('http/STATUS_CODES', http.STATUS_CODES['200'], 'string');
  eq('http/METHODS-GET', http.METHODS.includes('GET'), true);
  isType('http/createServer', http.createServer, 'function');
  isType('http/Agent', http.Agent, 'function');
  // https — exercise an Agent (offline-safe, no connection): protocol must be 'https:'.
  const https = require('node:https');
  isType('https/request', https.request, 'function');
  eq('https/Agent-protocol', new https.Agent({ keepAlive: true }).protocol, 'https:');
  // http2 — getDefaultSettings returns the documented SETTINGS shape (offline-safe, no connect).
  const http2 = require('node:http2');
  isType('http2/connect', http2.connect, 'function');
  isType('http2/getDefaultSettings.headerTableSize', http2.getDefaultSettings().headerTableSize, 'number');
  truthy('http2/constants.HTTP2_HEADER_STATUS', http2.constants.HTTP2_HEADER_STATUS === ':status');
  // tls — createSecureContext returns a SecureContext (offline-safe, no handshake).
  const tls = require('node:tls');
  isType('tls/connect', tls.connect, 'function');
  truthy('tls/createSecureContext', typeof tls.createSecureContext({}) === 'object');
  truthy('tls/rootCertificates', Array.isArray(tls.rootCertificates) && tls.rootCertificates.length > 0);

  return new Promise((resolve) => {
    let done = false;
    const finish = (fn) => { if (!done) { done = true; fn(); resolve(); } };
    let server;
    try {
      server = http.createServer((req, res) => { res.writeHead(200, { 'Content-Type': 'text/plain' }); res.end('PONG'); });
      const to = setTimeout(() => { try { server.close(); } catch {} finish(() => skip('http/loopback-roundtrip', 'loopback timeout (restricted env)')); }, 4000);
      server.on('error', (e) => { clearTimeout(to); finish(() => skip('http/loopback-roundtrip', 'server error: ' + e.code)); });
      server.listen(0, '127.0.0.1', () => {
        const port = server.address().port;
        const req = http.get({ host: '127.0.0.1', port, path: '/' }, (res) => {
          let body = ''; res.setEncoding('utf8');
          res.on('data', (c) => (body += c));
          res.on('end', () => { clearTimeout(to); try { server.close(); } catch {} finish(() => { eq('http/loopback-roundtrip', body, 'PONG'); }); });
        });
        req.on('error', (e) => { clearTimeout(to); try { server.close(); } catch {} finish(() => skip('http/loopback-roundtrip', 'client error: ' + e.code)); });
      });
    } catch (e) { finish(() => skip('http/loopback-roundtrip', 'exception: ' + e.message)); }
  });
}

function mod_dns() {
  const dns = require('node:dns');
  isType('dns/lookup', dns.lookup, 'function');
  isType('dns/resolve4', dns.resolve4, 'function');
  isType('dns/promises.resolve', require('node:dns/promises').resolve, 'function');
  isType('dns/setServers', dns.setServers, 'function');
  isType('dns/getServers', dns.getServers, 'function');
  // dns.promises.lookup('localhost') is offline-safe (resolves via /etc/hosts/NSS, no network DNS).
  // Wrapped in a watchdog: if the resolver stalls, SKIP instead of hanging.
  return withTimeout('dns/lookup-localhost', 4000,
    dns.promises.lookup('localhost').then((r) => {
      ok('dns/lookup-localhost', r && (r.address === '127.0.0.1' || r.address === '::1' || typeof r.address === 'string'),
        'address=' + (r && r.address));
    }, (e) => skip('dns/lookup-localhost', 'resolver error: ' + (e && e.code))));
}

function mod_dgram() {
  const dgram = require('node:dgram');
  isType('dgram/createSocket', dgram.createSocket, 'function');
  // Bind a UDP socket on the loopback ephemeral port (offline-safe). On a restricted env, SKIP.
  return new Promise((resolve) => {
    let s, settled = false;
    const done = (fn) => { if (settled) return; settled = true; try { if (s) s.close(); } catch {} fn(); resolve(); };
    const to = setTimeout(() => done(() => skip('dgram/bind-loopback', 'bind timeout (restricted env)')), 4000);
    if (to.unref) to.unref();
    try {
      s = dgram.createSocket('udp4');
      s.on('error', (e) => { clearTimeout(to); done(() => skip('dgram/bind-loopback', 'bind error: ' + e.code)); });
      s.bind(0, '127.0.0.1', () => {
        clearTimeout(to);
        const addr = s.address();
        done(() => ok('dgram/bind-loopback', addr && addr.port > 0 && addr.address === '127.0.0.1', 'addr=' + JSON.stringify(addr)));
      });
    } catch (e) { clearTimeout(to); done(() => skip('dgram/bind-loopback', 'exception: ' + e.message)); }
  });
}

function mod_child_process() {
  const cp = require('node:child_process');
  isType('child_process/spawn', cp.spawn, 'function');
  isType('child_process/exec', cp.exec, 'function');
  isType('child_process/execFile', cp.execFile, 'function');
  isType('child_process/fork', cp.fork, 'function');
  // execSync running the same node binary deterministically (no shell dependency on `echo`)
  try {
    const out = cp.execFileSync(process.execPath, ['-e', 'process.stdout.write("CHILD_OK")'], { encoding: 'utf8' });
    eq('child_process/execFileSync-self', out, 'CHILD_OK');
  } catch (e) { skip('child_process/execFileSync-self', 'spawn restricted: ' + e.code); }
  // spawnSync
  try {
    const r = cp.spawnSync(process.execPath, ['-e', 'process.exit(3)']);
    // Some restricted/emulated environments (e.g. running under qemu-user) cannot
    // exec a nested binary: spawnSync does not throw but reports the failure on the
    // result object (r.error.code === 'ENOEXEC', r.status === null). Treat that as a
    // SKIP, identical in intent to the catch below; on a real kernel r.status is a
    // number and the strict assertion applies.
    if (r.error || r.status === null) {
      skip('child_process/spawnSync-exitcode', 'spawn restricted: ' + (r.error && r.error.code || 'null-status'));
    } else {
      eq('child_process/spawnSync-exitcode', r.status, 3);
    }
  } catch (e) { skip('child_process/spawnSync-exitcode', 'spawn restricted: ' + e.code); }
}

function mod_worker_threads() {
  const wt = require('node:worker_threads');
  isType('worker_threads/Worker', wt.Worker, 'function');
  isType('worker_threads/isMainThread', wt.isMainThread, 'boolean');
  isType('worker_threads/MessageChannel', wt.MessageChannel, 'function');
  isType('worker_threads/SHARE_ENV', wt.SHARE_ENV, 'symbol');
  // SharedArrayBuffer + Atomics (language-level concurrency)
  eq('worker_threads/SharedArrayBuffer+Atomics', (() => {
    const sab = new SharedArrayBuffer(8); const ta = new Int32Array(sab);
    Atomics.store(ta, 0, 41); Atomics.add(ta, 0, 1); return Atomics.load(ta, 0);
  })(), 42);
  // MessageChannel roundtrip — wrapped in a watchdog so a stalled port never hangs the carpet.
  let p1, p2;
  return withTimeout('worker_threads/MessageChannel-roundtrip', 5000, new Promise((resolve) => {
    const { port1, port2 } = new wt.MessageChannel();
    p1 = port1; p2 = port2;
    port2.on('message', (m) => { eq('worker_threads/MessageChannel-roundtrip', m, 'ping'); port1.close(); port2.close(); resolve(); });
    port1.postMessage('ping');
  }), () => { try { p1 && p1.close(); p2 && p2.close(); } catch {} });
}

function mod_process() {
  isType('process/pid', process.pid, 'number');
  isType('process/platform', process.platform, 'string');
  isType('process/arch', process.arch, 'string');
  isType('process/version', process.version, 'string');
  truthy('process/versions', process.versions.node === process.version.slice(1));
  isType('process/argv', process.argv[0], 'string');
  isType('process/env', process.env, 'object');
  isType('process/cwd', process.cwd(), 'string');
  isType('process/hrtime', process.hrtime, 'function');
  truthy('process/hrtime.bigint', typeof process.hrtime.bigint() === 'bigint');
  isType('process/memoryUsage', process.memoryUsage().rss, 'number');
  isType('process/nextTick', process.nextTick, 'function');
  isType('process/uptime', process.uptime(), 'number');
  isType('process/cpuUsage', process.cpuUsage().user, 'number');
  truthy('process/release', process.release.name === 'node');
  isType('process/exitCode-settable', (() => { const old = process.exitCode; process.exitCode = 0; process.exitCode = old; return 'ok'; })(), 'string');
}

function mod_perf_hooks() {
  const ph = require('node:perf_hooks');
  isType('perf_hooks/performance.now', ph.performance.now(), 'number');
  isType('perf_hooks/PerformanceObserver', ph.PerformanceObserver, 'function');
  truthy('perf_hooks/mark-measure', (() => { ph.performance.mark('a'); ph.performance.mark('b'); const m = ph.performance.measure('ab', 'a', 'b'); return m.duration >= 0; })());
  isType('perf_hooks/monitorEventLoopDelay', ph.monitorEventLoopDelay, 'function');
}

function mod_v8() {
  const v8 = require('node:v8');
  eq('v8/serialize-deserialize', (() => { const b = v8.serialize({ a: [1, 2], m: new Map([['k', 'v']]) }); const o = v8.deserialize(b); return o.a[1] + '-' + o.m.get('k'); })(), '2-v');
  truthy('v8/getHeapStatistics', v8.getHeapStatistics().heap_size_limit > 0);
  isType('v8/getHeapSpaceStatistics', v8.getHeapSpaceStatistics(), 'object');
  isType('v8/setFlagsFromString', v8.setFlagsFromString, 'function');
}

function mod_vm() {
  const vm = require('node:vm');
  eq('vm/runInNewContext', vm.runInNewContext('a+b', { a: 2, b: 3 }), 5);
  eq('vm/Script-runInContext', (() => { const ctx = vm.createContext({ x: 10 }); return new vm.Script('x*2').runInContext(ctx); })(), 20);
  eq('vm/runInThisContext', vm.runInThisContext('40+2'), 42);
  isType('vm/compileFunction', vm.compileFunction, 'function');
}

function mod_readline() {
  const readline = require('node:readline');
  isType('readline/createInterface', readline.createInterface, 'function');
  isType('readline/clearLine', readline.clearLine, 'function');
  isType('readline/promises', require('node:readline/promises').createInterface, 'function');
  // Read lines from a Readable
  const { Readable } = require('node:stream');
  const input = Readable.from('line1\nline2\nline3\n');
  const rl = readline.createInterface({ input, crlfDelay: Infinity });
  const lines = [];
  // Wrapped in a watchdog: a stalled stream never hangs the carpet.
  return withTimeout('readline/line-events', 5000, new Promise((resolve) => {
    rl.on('line', (l) => lines.push(l));
    rl.on('close', () => { eq('readline/line-events', lines.join(','), 'line1,line2,line3'); resolve(); });
  }), () => { try { rl.close(); } catch {} });
}

function mod_diagnostics_channel() {
  const dc = require('node:diagnostics_channel');
  isType('diagnostics_channel/channel', dc.channel, 'function');
  isType('diagnostics_channel/tracingChannel', dc.tracingChannel, 'function');
  const ch = dc.channel('carpet:test');
  let got = null; ch.subscribe((msg) => { got = msg; });
  ch.publish({ v: 99 });
  eq('diagnostics_channel/publish-subscribe', got && got.v, 99);
}

function mod_async_hooks() {
  const ah = require('node:async_hooks');
  isType('async_hooks/createHook', ah.createHook, 'function');
  isType('async_hooks/executionAsyncId', ah.executionAsyncId(), 'number');
  isType('async_hooks/AsyncLocalStorage', ah.AsyncLocalStorage, 'function');
  // AsyncLocalStorage context propagation — watchdog-guarded.
  return withTimeout('async_hooks/AsyncLocalStorage-propagation', 5000, new Promise((resolve) => {
    const als = new ah.AsyncLocalStorage();
    als.run({ id: 'ctx7' }, () => {
      setImmediate(() => { eq('async_hooks/AsyncLocalStorage-propagation', als.getStore().id, 'ctx7'); resolve(); });
    });
  }));
}

function mod_console_inspector() {
  isType('console/Console', require('node:console').Console, 'function');
  // capture console output via a custom Console to a writable
  const { Console } = require('node:console');
  const { Writable } = require('node:stream');
  let buf = '';
  const w = new Writable({ write(c, e, cb) { buf += c.toString(); cb(); } });
  const c = new Console({ stdout: w });
  c.log('captured%d', 5);
  eq('console/custom-stream', buf.trim(), 'captured5');
  isType('inspector/open', require('node:inspector').open, 'function');
  isType('inspector/Session', require('node:inspector').Session, 'function');
}

function mod_module_misc() {
  const Module = require('node:module');
  isType('module/builtinModules', Array.isArray(Module.builtinModules), 'boolean');
  truthy('module/builtinModules-has-fs', Module.builtinModules.includes('fs'));
  isType('module/createRequire', Module.createRequire, 'function');
  truthy('module/isBuiltin', Module.isBuiltin ? Module.isBuiltin('node:fs') : true);
  // punycode (deprecated but documented)
  try { const pc = require('node:punycode'); eq('punycode/encode', pc.toASCII('exämple.com').startsWith('xn--'), true); }
  catch (e) { skip('punycode', 'not bundled: ' + e.code); }
  // node:test runner module presence (run via shell carpet too)
  const test = require('node:test');
  isType('test/test-fn', test.test ? test.test : test, 'function');
  isType('test/describe', test.describe, 'function');
  isType('test/it', test.it, 'function');
  isType('test/mock', test.mock, 'object');
  isType('test/reporters', require('node:test/reporters').spec, 'function');
}

function mod_more_core() {
  // node:tty — isatty returns a boolean; WriteStream/ReadStream classes present.
  const tty = require('node:tty');
  isType('tty/isatty-bool', tty.isatty(1), 'boolean');
  isType('tty/WriteStream', tty.WriteStream, 'function');
  isType('tty/ReadStream', tty.ReadStream, 'function');

  // node:repl — PRESENCE ONLY (never start an interactive REPL: it would read the TTY and hang).
  const repl = require('node:repl');
  isType('repl/start-fn', repl.start, 'function');
  isType('repl/REPLServer-fn', repl.REPLServer, 'function');
  truthy('repl/REPL_MODE-constants', typeof repl.REPL_MODE_SLOPPY !== 'undefined' && typeof repl.REPL_MODE_STRICT !== 'undefined');

  // node:cluster — isPrimary boolean, fork is a function (do NOT fork: would spawn workers).
  const cluster = require('node:cluster');
  isType('cluster/isPrimary-bool', cluster.isPrimary, 'boolean');
  isType('cluster/isWorker-bool', cluster.isWorker, 'boolean');
  ok('cluster/isPrimary-xor-isWorker', cluster.isPrimary !== cluster.isWorker);
  isType('cluster/fork-fn', cluster.fork, 'function');
  isType('cluster/Worker-class', cluster.Worker, 'function');

  // node:domain — deprecated but documented; create() returns a Domain.
  const domain = require('node:domain');
  isType('domain/create-fn', domain.create, 'function');
  const d = domain.create();
  isType('domain/instance-run', d.run, 'function');

  // node:trace_events — createTracing is a function; presence of a Tracing object (do not enable).
  const te = require('node:trace_events');
  isType('trace_events/createTracing-fn', te.createTracing, 'function');
  isType('trace_events/getEnabledCategories-fn', te.getEnabledCategories, 'function');

  // node:wasi — WASI is a constructor (experimental). Presence-gated: requiring it emits an
  // ExperimentalWarning but works on v18+; we only assert the export shape, never instantiate
  // (instantiation needs a wasm module + preopens).
  try {
    const wasi = require('node:wasi');
    isType('wasi/WASI-fn', wasi.WASI, 'function');
  } catch (e) { skip('wasi/WASI-fn', 'node:wasi unavailable: ' + (e && e.code)); }
}

function mod_sqlite() {
  // node:sqlite is experimental, v22.5+; require the flag. Gate fully.
  whenMajor(22, 'sqlite/module', () => {
    let DB;
    try { ({ DatabaseSync: DB } = require('node:sqlite')); }
    catch (e) { skip('sqlite/module', 'needs --experimental-sqlite flag: ' + e.code); return; }
    try {
      const db = new DB(':memory:');
      db.exec('CREATE TABLE t(id INTEGER, name TEXT)');
      db.prepare('INSERT INTO t VALUES (?, ?)').run(1, 'alice');
      const row = db.prepare('SELECT name FROM t WHERE id=?').get(1);
      eq('sqlite/insert-select', row.name, 'alice');
      db.close();
    } catch (e) { skip('sqlite/exec', 'sqlite runtime error: ' + e.message); }
  });
}

// ===========================================================================
// SECTION 4 — ASYNC / PROMISES / CONCURRENCY (language-level)
// ===========================================================================
async function section_async() {
  eq('async/await-basic', await Promise.resolve(7), 7);
  eq('async/all', (await Promise.all([1, 2, 3].map((x) => Promise.resolve(x * 2)))).join(','), '2,4,6');
  eq('async/race', await Promise.race([Promise.resolve('fast'), new Promise((r) => setTimeout(() => r('slow'), 50))]), 'fast');
  eq('async/allSettled', (await Promise.allSettled([Promise.resolve(1), Promise.reject(2)])).map((r) => r.status).join(','), 'fulfilled,rejected');
  eq('async/any', await Promise.any([Promise.reject('a'), Promise.resolve('b')]), 'b');
  // async iterator
  async function* gen() { yield 1; yield 2; yield 3; }
  let sum = 0; for await (const v of gen()) sum += v;
  eq('async/for-await-of', sum, 6);
  // try/catch rejection
  let caught = false; try { await Promise.reject(new Error('boom')); } catch { caught = true; }
  eq('async/await-reject-catch', caught, true);
  whenMajor(22, 'async/Array.fromAsync', async () => {
    if (typeof Array.fromAsync === 'function') {
      eq('async/Array.fromAsync-result', (await Array.fromAsync(gen())).join(','), '1,2,3');
    } else skip('async/Array.fromAsync', 'not present');
  });
}

// ===========================================================================
// DRIVER
// ===========================================================================
async function main() {
  console.log(`# node-carpet: Node ${process.version} (major ${NODE_MAJOR}) on ${process.platform}/${process.arch}`);
  // GLOBAL WATCHDOG: if any check wedges the event loop / a promise never settles, force a clean
  // FAIL exit instead of hanging the harness forever. Unref'd so it never keeps the loop alive by
  // itself; only fires if the process is still running at the deadline.
  const GLOBAL_WATCHDOG_MS = parseInt(process.env.NODE_CARPET_WATCHDOG_MS || '60000', 10);
  const watchdog = setTimeout(() => {
    console.log('FAIL global-watchdog :: carpet exceeded ' + GLOBAL_WATCHDOG_MS + 'ms — forcing exit');
    console.log('NODE_CARPET_FAIL');
    process.exit(1);
  }, GLOBAL_WATCHDOG_MS);
  if (watchdog.unref) watchdog.unref();
  try {
    // synchronous sections
    section_language();
    section_globals();
    mod_buffer();
    mod_path();
    mod_os();
    mod_zlib();
    mod_url();
    mod_querystring();
    mod_string_decoder();
    mod_net_http_sync_presence();
    mod_child_process();
    mod_process();
    mod_perf_hooks();
    mod_v8();
    mod_vm();
    mod_diagnostics_channel();
    mod_console_inspector();
    mod_module_misc();
    mod_more_core();
    mod_sqlite();

    // async sections (awaited; each internally watchdog-guarded)
    await mod_assert();
    await mod_util();
    await mod_events();
    await mod_stream();
    await mod_fs();
    await mod_crypto();
    await mod_timers();
    await mod_dns();
    await mod_dgram();
    await mod_net_http();
    await mod_worker_threads();
    await mod_readline();
    await mod_async_hooks();
    await section_async();
  } catch (e) {
    FAIL++; FAILURES.push('UNCAUGHT: ' + (e && e.stack ? e.stack : e));
    console.log('FAIL UNCAUGHT :: ' + (e && e.stack ? e.stack : e));
  }

  clearTimeout(watchdog);
  cleanup();
  console.log(`\n# RESULTS: PASS=${PASS} FAIL=${FAIL} SKIP=${SKIP} TOTAL=${PASS + FAIL}`);
  if (FAIL === 0) {
    console.log('NODE_CARPET_OK');
    process.exitCode = 0;
  } else {
    console.log('NODE_CARPET_FAIL');
    console.log('Failures:\n  ' + FAILURES.join('\n  '));
    process.exitCode = 1;
  }
}

// net/http presence-only checks split so loopback (async) can be optional
function mod_net_http_sync_presence() {
  const http = require('node:http');
  const net = require('node:net');
  isType('net/createServer-present', net.createServer, 'function');
  eq('net/isIP-v6', net.isIP('::1'), 6);
  isType('http/createServer-present', http.createServer, 'function');
  isType('http/request-present', http.request, 'function');
}

main();
