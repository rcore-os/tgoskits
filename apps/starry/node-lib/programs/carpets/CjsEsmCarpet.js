'use strict';
// INDUSTRIAL carpet for CommonJS <-> ESM interop on Node.js v22.22.2.
// This file itself is CommonJS (.js). It writes a self-contained fixture tree
// under <__dirname>/tmp-cjsesm (.mjs / .cjs / package.json type:module subdir /
// a tiny exports-conditioned package) and exercises every documented interop
// path between the two module systems on this exact Node version:
//   - require() of .cjs (object-exports + function-exports) and JSON
//   - dynamic import() of .mjs from CJS (default + named bindings)
//   - import() of .cjs from ESM (default = module.exports; named via the
//     cjs-module-lexer named-export detection, with its exact detection rules)
//   - __esModule interop (native node does NOT auto-unwrap, unlike bundlers)
//   - data: URL import (default + named)
//   - import.meta.url / dirname / filename are real file: URLs
//   - createRequire(), node:module builtins API, require(esm) (v22 feature)
//   - package.json "type":"module" subdir + "exports" import/require conditions
//   - circular require partial-exports resolution
//   - CJS-only globals (structuredClone / TextEncoder / TextDecoder)
//   - real vendored CJS packages resolve from node_modules
// Every assertion is an exact-value check against a golden observed by running
// node v22.22.2 directly. Deterministic only: no clock, no random, no network.
// Portable: every path derives from __dirname; subprocess (none here) would use
// process.execPath. The fixture tree is removed best-effort at the end.

const fs = require('fs');
const path = require('path');
const url = require('node:url');
const Module = require('node:module');
const { createRequire } = Module;

// ---- self-check harness (mirrors the delivered node-web carpets) ----------
let ok = 0, fail = 0;
function chk(cond, name) {
  if (cond) { ok++; } else { fail++; console.log('FAIL ' + name); }
}
function eq(actual, expected, name) {
  const c = actual === expected;
  if (!c) console.log('  expected=' + JSON.stringify(expected) + ' actual=' + JSON.stringify(actual));
  chk(c, name);
}

// ---- deterministic on-disk fixture tree (all under __dirname) --------------
const FIX = path.join(__dirname, 'tmp-cjsesm');
const NM = path.join(FIX, 'node_modules');
const DUAL = path.join(NM, 'dual');
const ESMPKG = path.join(FIX, 'esmpkg');

function rmrf(p) { try { fs.rmSync(p, { recursive: true, force: true }); } catch (e) { /* best-effort */ } }

rmrf(FIX);
fs.mkdirSync(FIX, { recursive: true });
fs.mkdirSync(DUAL, { recursive: true });
fs.mkdirSync(ESMPKG, { recursive: true });

const W = (rel, body) => fs.writeFileSync(path.join(FIX, rel), body);

// -- ESM module: named + default + import.meta export
W('m.mjs',
  'export const NAMED = \'named-val\';\n' +
  'export function add(a, b) { return a + b; }\n' +
  'export default { kind: \'esm-default\', n: 7 };\n' +
  'export const META = import.meta.url;\n');

// -- ESM module that re-exports its own import.meta coordinates
W('meta.mjs',
  'export const url = import.meta.url;\n' +
  'export const dir = import.meta.dirname;\n' +
  'export const fname = import.meta.filename;\n');

// -- CJS: object module.exports (with a value-bearing object literal) + late prop
//    lexer detects ONLY `module.exports.added` (object-literal w/ values is opaque)
W('c.cjs',
  'module.exports = { foo: 1, bar: \'two\', fn: function () { return \'fnret\'; } };\n' +
  'module.exports.added = \'late\';\n');

// -- CJS: exports.X assignments (both detected by lexer)
W('c2.cjs',
  'exports.alpha = \'A\';\n' +
  'exports.beta = 42;\n');

// -- CJS: object literal SHORTHAND (identifiers) -> lexer detects a, b
W('lex1.cjs',
  'const a = 1, b = 2;\n' +
  'module.exports = { a, b };\n');

// -- CJS: object literal with VALUES -> lexer detects NOTHING (only default)
W('lex2.cjs',
  'module.exports = { p: 1, q: 2 };\n');

// -- CJS: exports.one + Object.defineProperty -> both detected
W('lex3.cjs',
  'exports.one = \'I\';\n' +
  'Object.defineProperty(exports, \'two\', { enumerable: true, value: \'II\' });\n');

// -- CJS: babel-style __esModule + default + named (native node: NO unwrap)
W('esm_interop.cjs',
  'exports.default = \'the-default\';\n' +
  'exports.named = \'the-named\';\n' +
  'exports.__esModule = true;\n');

// -- CJS: function as module.exports + a late prop + a prop on the local fn
W('funcmod.cjs',
  'function greet(name) { return \'hi \' + name; }\n' +
  'greet.version = \'2.0\';\n' +
  'module.exports = greet;\n' +
  'module.exports.extra = \'X\';\n');

// -- JSON module
W('data.json', '{"x":10,"y":[1,2,3]}\n');

// -- anchor file for createRequire so resolution is rooted in the fixture dir
W('anchor.cjs', 'module.exports = true;\n');

// -- circular require pair
W('circA.cjs',
  'exports.nameA = \'A\';\n' +
  'const b = require(\'./circB.cjs\');\n' +
  'exports.fromB_atLoad = b.nameB;\n' +
  'exports.bDoneFlag = b.done;\n' +
  'exports.done = true;\n');
W('circB.cjs',
  'exports.nameB = \'B\';\n' +
  'const a = require(\'./circA.cjs\');\n' +
  'exports.aPartial_nameA = a.nameA;\n' +
  'exports.aPartial_done = a.done;\n' +   // A not finished yet -> undefined (warns on stderr)
  'exports.done = true;\n');

// -- subdir treated as ESM purely via package.json {"type":"module"}
fs.writeFileSync(path.join(ESMPKG, 'package.json'), '{ "type": "module" }\n');
fs.writeFileSync(path.join(ESMPKG, 'index.js'),
  'export const flavor = \'esm-via-pkgjson\';\n' +
  'export default 123;\n');

// -- dual package with conditional "exports" (import vs require) + subpath
fs.writeFileSync(path.join(DUAL, 'package.json'),
  '{ "name": "dual", "version": "1.0.0", "exports": { ".": { "import": "./esm.mjs", "require": "./cjs.cjs" }, "./sub": "./sub.cjs" } }\n');
fs.writeFileSync(path.join(DUAL, 'esm.mjs'),
  'export const side = \'import-condition\';\nexport default \'esm-default\';\n');
fs.writeFileSync(path.join(DUAL, 'cjs.cjs'),
  'module.exports = { side: \'require-condition\' };\n');
fs.writeFileSync(path.join(DUAL, 'sub.cjs'),
  'exports.subval = \'subpath\';\n');

// -- ESM fixture that resolves the dual package from inside the fixture tree
//    (bare-specifier resolution is anchored at this module's URL)
W('condtest.mjs',
  'import { createRequire } from \'node:module\';\n' +
  'const require = createRequire(import.meta.url);\n' +
  'export const reqSide = require(\'dual\').side;\n' +
  'const imp = await import(\'dual\');\n' +
  'export const impSide = imp.side;\n' +
  'export const impDefault = imp.default;\n' +
  'const sub = await import(\'dual/sub\');\n' +
  'export const subVal = sub.subval;\n');

// require() rooted in the fixture dir, and file:-URL importer for the fixtures
const fixRequire = createRequire(path.join(FIX, 'anchor.cjs'));
const fileURL = (rel) => url.pathToFileURL(path.join(FIX, rel)).href;
const imp = (rel) => import(fileURL(rel));

async function main() {
  // ===== Section A: node:module builtins & Module API =====================
  eq(require('node:module'), require('module'), 'A.node:module===module');     // 1
  eq(typeof Module.createRequire, 'function', 'A.createRequire.fn');           // 2
  chk(Array.isArray(Module.builtinModules), 'A.builtinModules.array');        // 3
  eq(Module.builtinModules.includes('fs'), true, 'A.builtin.fs');             // 4
  eq(Module.builtinModules.includes('os'), true, 'A.builtin.os');             // 5
  eq(Module.builtinModules.includes('node:fs'), false, 'A.builtin.no-prefix');// 6
  eq(Module.isBuiltin('fs'), true, 'A.isBuiltin.fs');                         // 7
  eq(Module.isBuiltin('node:fs'), true, 'A.isBuiltin.node:fs');              // 8
  eq(Module.isBuiltin('nonexistent-xyz'), false, 'A.isBuiltin.no');           // 9
  eq(module instanceof Module, true, 'A.module.instanceof');                  // 10
  eq(typeof module.id, 'string', 'A.module.id.type');                         // 11
  eq(module.id, '.', 'A.module.id.dot');                                       // 12
  eq(require.main === module, true, 'A.require.main');                         // 13
  eq(typeof require.cache, 'object', 'A.require.cache');                       // 14

  // ===== Section B: CJS globals (deterministic) ===========================
  eq(typeof structuredClone, 'function', 'B.structuredClone.fn');             // 15
  eq(JSON.stringify(structuredClone({ n: [1, 2] })), '{"n":[1,2]}', 'B.sc'); // 16
  eq(typeof TextEncoder, 'function', 'B.TextEncoder.fn');                     // 17
  eq(Array.from(new TextEncoder().encode('AB')).join(','), '65,66', 'B.te'); // 18
  eq(typeof TextDecoder, 'function', 'B.TextDecoder.fn');                     // 19
  eq(new TextDecoder().decode(new Uint8Array([65, 66])), 'AB', 'B.td');       // 20
  eq(typeof Buffer, 'function', 'B.Buffer.global');                           // 21
  eq(typeof queueMicrotask, 'function', 'B.queueMicrotask');                  // 22

  // ===== Section C: __dirname / __filename in CJS =========================
  eq(typeof __dirname, 'string', 'C.dirname.type');                           // 23
  eq(path.basename(__filename), 'CjsEsmCarpet.js', 'C.filename.base');        // 24
  eq(__dirname, path.dirname(__filename), 'C.dir-is-dirname');                // 25

  // ===== Section D: require() of .cjs (object module.exports) ==============
  const c = fixRequire('./c.cjs');
  eq(c.foo, 1, 'D.c.foo');                                                    // 26
  eq(c.bar, 'two', 'D.c.bar');                                                // 27
  eq(c.added, 'late', 'D.c.added');                                           // 28
  eq(c.fn(), 'fnret', 'D.c.fn');                                              // 29

  // ===== Section E: require() of .cjs (function module.exports) ============
  const fm = fixRequire('./funcmod.cjs');
  eq(typeof fm, 'function', 'E.fm.type');                                     // 30
  eq(fm('bob'), 'hi bob', 'E.fm.call');                                       // 31
  eq(fm.version, '2.0', 'E.fm.version');                                      // 32
  eq(fm.extra, 'X', 'E.fm.extra');                                            // 33

  // ===== Section F: require() of JSON =====================================
  const jraw = fixRequire('./data.json');
  eq(jraw.x, 10, 'F.json.x');                                                 // 34
  eq(JSON.stringify(jraw.y), '[1,2,3]', 'F.json.y');                          // 35

  // ===== Section G: require.resolve ========================================
  eq(path.basename(fixRequire.resolve('./c.cjs')), 'c.cjs', 'G.resolve.base');// 36
  eq(require.resolve('less').includes('/node_modules/less/'), true, 'G.resolve.nm'); // 37

  // ===== Section H: real vendored CJS packages require cleanly =============
  eq(typeof require('less').render, 'function', 'H.less.render');             // 38
  eq(typeof require('sass').compileString, 'function', 'H.sass.compile');     // 39
  eq(typeof require('terser').minify, 'function', 'H.terser.minify');         // 40
  eq(typeof require('stylus'), 'function', 'H.stylus.fn');                    // 41
  eq(require('@babel/core').version, '7.26.0', 'H.babel.version');            // 42

  // ===== Section I: dynamic import() of .mjs from CJS =====================
  const m = await imp('m.mjs');
  eq(Object.keys(m).sort().join(','), 'META,NAMED,add,default', 'I.m.keys');  // 43
  eq(m.NAMED, 'named-val', 'I.m.named');                                      // 44
  eq(m.add(2, 3), 5, 'I.m.fn');                                               // 45
  eq(m.default.kind, 'esm-default', 'I.m.default.kind');                      // 46
  eq(m.default.n, 7, 'I.m.default.n');                                        // 47
  eq(m.META.startsWith('file:'), true, 'I.m.meta.file');                      // 48

  // ===== Section J: import.meta.url / dirname / filename are file: URLs ====
  const metaPath = path.join(FIX, 'meta.mjs');
  const meta = await imp('meta.mjs');
  eq(meta.url, url.pathToFileURL(metaPath).href, 'J.meta.url.exact');         // 49
  eq(meta.url.startsWith('file://'), true, 'J.meta.url.scheme');              // 50
  eq(meta.url.endsWith('/meta.mjs'), true, 'J.meta.url.suffix');              // 51
  eq(meta.dir, FIX, 'J.meta.dirname');                                        // 52
  eq(meta.fname, metaPath, 'J.meta.filename');                               // 53

  // ===== Section K: import() of .cjs from ESM (default=module.exports) =====
  const ci = await imp('c.cjs');
  // lexer detected ONLY `added` (object literal with values is opaque)
  eq(Object.keys(ci).sort().join(','), 'added,default', 'K.c.keys');          // 54
  eq(ci.added, 'late', 'K.c.named.added');                                    // 55
  eq(ci.foo, undefined, 'K.c.foo.undetected');                               // 56
  eq(ci.default.foo, 1, 'K.c.default.foo');                                   // 57
  eq(ci.default.bar, 'two', 'K.c.default.bar');                              // 58
  eq(ci.default.added, 'late', 'K.c.default.added');                         // 59
  eq(typeof ci.default.fn, 'function', 'K.c.default.fn');                     // 60

  const c2 = await imp('c2.cjs');
  eq(Object.keys(c2).sort().join(','), 'alpha,beta,default', 'K.c2.keys');    // 61
  eq(c2.alpha, 'A', 'K.c2.alpha');                                            // 62
  eq(c2.beta, 42, 'K.c2.beta');                                               // 63
  eq(c2.default.alpha, 'A', 'K.c2.default.alpha');                           // 64

  // ===== Section L: exact cjs-module-lexer detection rules =================
  const l1 = await imp('lex1.cjs');   // shorthand identifiers -> detected
  eq(Object.keys(l1).sort().join(','), 'a,b,default', 'L.lex1.keys');         // 65
  eq(l1.a, 1, 'L.lex1.a');                                                    // 66
  eq(l1.b, 2, 'L.lex1.b');                                                    // 67
  const l2 = await imp('lex2.cjs');   // values -> NOT detected
  eq(Object.keys(l2).sort().join(','), 'default', 'L.lex2.keys');             // 68
  eq(l2.p, undefined, 'L.lex2.p.undetected');                                // 69
  eq(l2.default.p, 1, 'L.lex2.default.p');                                    // 70
  const l3 = await imp('lex3.cjs');   // exports.one + defineProperty -> both
  eq(Object.keys(l3).sort().join(','), 'default,one,two', 'L.lex3.keys');     // 71
  eq(l3.one, 'I', 'L.lex3.one');                                              // 72
  eq(l3.two, 'II', 'L.lex3.two');                                             // 73

  // ===== Section M: __esModule interop -- native node does NOT unwrap ======
  const ei = await imp('esm_interop.cjs');
  eq(Object.keys(ei).sort().join(','), '__esModule,default,named', 'M.keys'); // 74
  // `default` binding is the WHOLE module.exports, NOT exports.default:
  eq(ei.default.default, 'the-default', 'M.default-not-unwrapped');           // 75
  eq(ei.default.__esModule, true, 'M.default.__esModule');                    // 76
  eq(ei.named, 'the-named', 'M.named');                                       // 77
  eq(ei.__esModule, true, 'M.__esModule.named');                             // 78

  // ===== Section N: import() of function module from ESM ==================
  const fi = await imp('funcmod.cjs');
  eq(Object.keys(fi).sort().join(','), 'default,extra', 'N.keys');            // 79
  eq(typeof fi.default, 'function', 'N.default.fn');                          // 80
  eq(fi.default('amy'), 'hi amy', 'N.default.call');                          // 81
  eq(fi.extra, 'X', 'N.extra.detected');                                      // 82
  eq(fi.version, undefined, 'N.version.undetected');                          // 83

  // ===== Section O: data: URL import (default + named) ====================
  const d1 = await import('data:text/javascript,export default 42');
  eq(d1.default, 42, 'O.data.default');                                       // 84
  const d2 = await import('data:text/javascript,export const a=1;export default 99;export function f(){return 5}');
  eq(d2.a, 1, 'O.data.named.a');                                              // 85
  eq(d2.default, 99, 'O.data.named.default');                                // 86
  eq(d2.f(), 5, 'O.data.named.fn');                                           // 87

  // ===== Section P: package.json "type":"module" subdir ===================
  const sub = await imp('esmpkg/index.js');
  eq(sub.flavor, 'esm-via-pkgjson', 'P.subdir.named');                        // 88
  eq(sub.default, 123, 'P.subdir.default');                                   // 89

  // ===== Section Q: "exports" import/require conditions + subpath =========
  const cond = await imp('condtest.mjs');
  eq(cond.reqSide, 'require-condition', 'Q.require.condition');               // 90
  eq(cond.impSide, 'import-condition', 'Q.import.condition');                 // 91
  eq(cond.impDefault, 'esm-default', 'Q.import.default');                     // 92
  eq(cond.subVal, 'subpath', 'Q.subpath.export');                            // 93

  // ===== Section R: circular require -> partial exports ===================
  const A = fixRequire('./circA.cjs');
  const B = fixRequire('./circB.cjs');
  eq(A.nameA, 'A', 'R.A.nameA');                                              // 94
  eq(A.done, true, 'R.A.done');                                               // 95
  eq(A.fromB_atLoad, 'B', 'R.A.sees.full.B');                                 // 96
  eq(A.bDoneFlag, true, 'R.A.sees.B.done');                                   // 97
  eq(B.nameB, 'B', 'R.B.nameB');                                              // 98
  eq(B.aPartial_nameA, 'A', 'R.B.sees.partial.A.name');                       // 99
  eq(B.aPartial_done, undefined, 'R.B.sees.A.unfinished');                    // 100
  eq(B.done, true, 'R.B.done');                                               // 101

  // ===== Section S: createRequire rooted at a fixture path =================
  const r2 = createRequire(path.join(FIX, 'anchor.cjs'));
  eq(r2('node:path').sep, '/', 'S.createRequire.builtin');                    // 102
  eq(r2('./c2.cjs').alpha, 'A', 'S.createRequire.relative');                  // 103
  // createRequire also accepts a file: URL string
  const r3 = createRequire(url.pathToFileURL(path.join(FIX, 'anchor.cjs')).href);
  eq(r3('./data.json').x, 10, 'S.createRequire.fileurl');                     // 104

  // ===== Section T: require(esm) -- synchronous ESM via require (v22) ======
  const reqEsm = fixRequire('./m.mjs');
  eq(reqEsm.NAMED, 'named-val', 'T.require-esm.named');                       // 105
  eq(reqEsm.add(2, 3), 5, 'T.require-esm.fn');                                // 106
  eq(reqEsm.default.kind, 'esm-default', 'T.require-esm.default');            // 107
  eq(createRequire(path.join(FIX, 'anchor.cjs'))('./m.mjs').NAMED, 'named-val', 'T.createRequire-esm'); // 108
}

main()
  .then(() => {
    rmrf(FIX); // best-effort cleanup
    console.log('CJSESM_RESULT ok=' + ok + ' fail=' + fail);
    if (fail === 0) console.log('CJSESM_DONE');
    process.exit(fail === 0 ? 0 : 1);
  })
  .catch((e) => {
    fail++;
    console.log('FAIL exception: ' + (e && e.stack ? e.stack : e));
    rmrf(FIX);
    console.log('CJSESM_RESULT ok=' + ok + ' fail=' + fail);
    process.exit(1);
  });
