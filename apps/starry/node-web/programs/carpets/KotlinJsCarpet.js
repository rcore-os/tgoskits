// KotlinJsCarpet.js — carpet for the Kotlin/JS delivery on Node.js 22 LTS (StarryOS target runtime).
//
// Runs the Kotlin->JS module (kotlin-app.js, generated in prebuild from the committed Kotlin
// source assets/kotlin-app.kt by the Kotlin 2.0.21 JS/IR backend with -module-kind commonjs; see
// programs/SOURCES.md) as a child process, captures its stdout/stderr/exit, and asserts the output
// is byte-identical to the golden kotlin-REF.out with thorough per-line and per-token checks. The
// module self-executes its `main` on load (writes 6 lines via console.log).
//
// Kotlin language/stdlib features exercised by kotlin-app.js (reflected in the 6 golden lines):
//   - data classes + componentN/copy  -> points=(21,12);(23,14);(25,16)
//   - sealed class + exhaustive `when` -> areas=12,12,3 total=27
//   - higher-order fns / lambdas / map/filter/sum -> evens^2 sum=220
//   - generics + extension functions   -> second-of=b
//   - recursion (tail/plain)           -> fib(15)=610
//   - null-safety (?., ?:, !!)         -> nullsafe=YES
//
// Deterministic: no Date/Math.random/network/timestamps. Pure node + child process on the same
// Node 22 LTS binary. Marker: KOTLINJS_DONE printed ONLY when fail===0.

'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync, execFileSync } = require('child_process');

let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }

const DIR = __dirname;                 // carpet runs from its own dir (target: /root/nweb)
const NODE = process.execPath;         // the running node binary (target's musl node), not a hardcoded host path
const APP = path.join(DIR, 'kotlin-app.js');
const REF = path.join(DIR, 'kotlin-REF.out');

// --- Section 0: environment / target sanity ---------------------------------
// Node 22 LTS API surface — assert the MAJOR (the reproducible prebuild apk-adds the current
// v3.22 nodejs, whose patch level floats: v22.22.x today, a later 22.x tomorrow). The Kotlin/JS
// commonjs module uses no version-specific node API, so any Node >= 22 runs it identically.
chk(parseInt(process.versions.node.split('.')[0], 10) >= 22, 'node major >= 22 (Node 22 LTS; got ' + process.version + ')');
chk(fs.existsSync(NODE), 'host verify binary exists: ' + NODE);
chk(fs.existsSync(APP), 'kotlin-app.js exists');
chk(fs.existsSync(REF), 'kotlin-REF.out exists');

// --- Section 1: the IR-emitted module is the full module, not a stub --------
const appStat = fs.statSync(APP);
chk(appStat.isFile(), 'kotlin-app.js is a regular file');
chk(appStat.size > 100000, 'kotlin-app.js size > 100000 bytes (got ' + appStat.size + ')');
// kotlin-app.js is regenerated in prebuild from the committed kotlin-app.kt by the pinned Kotlin
// 2.0.21 JS/IR compiler (NOT committed). The whole-program commonjs emit inlines the Kotlin
// stdlib, so the file is ~640-660 KB; assert a tight range (rejects a DCE'd stub or a truncated
// file) rather than a single exact byte count, which is specific to one compiler build and would
// break on a Kotlin patch bump.
chk(appStat.size > 500000 && appStat.size < 800000, 'kotlin-app.js size in [500000,800000) bytes (whole-program stdlib inline; got ' + appStat.size + ')');

const appSrc = fs.readFileSync(APP, 'utf8');
// Hallmarks of the Kotlin 2.0.21 IR backend + commonjs module-kind.
chk(appSrc.indexOf('//region block: polyfills') !== -1, 'app has IR polyfills region header');
chk(appSrc.indexOf('Math.imul') !== -1, 'app references Math.imul polyfill (IR backend)');
chk(/module\.exports|exports\./.test(appSrc), 'app uses commonjs exports (-module-kind commonjs)');

// --- Section 2: golden bytes ------------------------------------------------
const refBuf = fs.readFileSync(REF);                 // raw bytes
const refText = refBuf.toString('utf8');
chk(refBuf.length === 107, 'golden byte length === 107 (got ' + refBuf.length + ')');
chk(refBuf[refBuf.length - 1] === 0x0a, 'golden ends with a single LF (0x0a)');
chk(refBuf.indexOf(0x0d) === -1, 'golden contains no CR bytes (pure LF, no CRLF)');

// Exact golden lines, hard-coded (independent ground truth from the spec).
const EXPECTED_LINES = [
  'points=(21,12);(23,14);(25,16)',
  'areas=12,12,3 total=27',
  'evens^2 sum=220',
  'second-of=b',
  'fib(15)=610',
  'nullsafe=YES',
];
const EXPECTED_TEXT = EXPECTED_LINES.join('\n') + '\n';
chk(refText === EXPECTED_TEXT, 'golden text === expected 6 lines + trailing LF');
chk(EXPECTED_TEXT.length === 107, 'expected text length === 107 chars');

// --- Section 3: run the module as a child, capture stdout/stderr/exit -------
// spawnSync with no encoding -> Buffers (for byte-exact comparison).
const run = spawnSync(NODE, [APP], { cwd: DIR });
chk(run.error === undefined || run.error === null, 'child spawned without error');
chk(run.status === 0, 'child exit status === 0 (got ' + run.status + ')');
chk(run.signal === null, 'child terminated by no signal');

const outBuf = run.stdout;                            // Buffer
const errBuf = run.stderr;                            // Buffer
chk(Buffer.isBuffer(outBuf), 'stdout captured as Buffer');
chk(errBuf.length === 0, 'stderr is empty (got ' + errBuf.length + ' bytes)');

// --- Section 4: byte-identical stdout vs golden -----------------------------
chk(outBuf.length === 107, 'stdout byte length === 107 (got ' + outBuf.length + ')');
chk(Buffer.compare(outBuf, refBuf) === 0, 'stdout is byte-identical to golden (Buffer.compare===0)');
const outText = outBuf.toString('utf8');
chk(outText === refText, 'stdout text === golden text');
chk(outText === EXPECTED_TEXT, 'stdout text === expected hard-coded text');
chk(outBuf[outBuf.length - 1] === 0x0a, 'stdout ends with a single LF');
chk(outBuf.indexOf(0x0d) === -1, 'stdout contains no CR bytes');

// Cross-check via execFileSync (second independent capture path).
const execOut = execFileSync(NODE, [APP], { cwd: DIR }); // Buffer
chk(Buffer.compare(execOut, refBuf) === 0, 'execFileSync stdout byte-identical to golden');

// --- Section 5: per-line assertions -----------------------------------------
// Split on LF; trailing LF yields an empty final element -> drop it.
const parts = outText.split('\n');
chk(parts.length === 7, 'split on LF yields 7 parts (6 lines + trailing empty)');
chk(parts[6] === '', 'final split element is empty (trailing newline present)');
const lines = parts.slice(0, 6);
chk(lines.length === 6, 'line count === 6');

chk(lines[0] === 'points=(21,12);(23,14);(25,16)', 'line 1 exact: points=(21,12);(23,14);(25,16)');
chk(lines[1] === 'areas=12,12,3 total=27',          'line 2 exact: areas=12,12,3 total=27');
chk(lines[2] === 'evens^2 sum=220',                 'line 3 exact: evens^2 sum=220');
chk(lines[3] === 'second-of=b',                     'line 4 exact: second-of=b');
chk(lines[4] === 'fib(15)=610',                     'line 5 exact: fib(15)=610');
chk(lines[5] === 'nullsafe=YES',                    'line 6 exact: nullsafe=YES');

// Each emitted line must equal its expected counterpart (loop, defensive).
for (let i = 0; i < 6; i++) {
  chk(lines[i] === EXPECTED_LINES[i], 'line ' + (i + 1) + ' === EXPECTED_LINES[' + i + ']');
}

// --- Section 6: token-level assertions (Kotlin feature semantics) -----------
// Line 1: data class Point(x,y) scaled/translated, joined ";" with toString "(x,y)".
const ptTokens = lines[0].slice('points='.length).split(';');
chk(lines[0].startsWith('points='), 'line 1 starts with points=');
chk(ptTokens.length === 3, 'line 1 has 3 point tokens');
chk(ptTokens[0] === '(21,12)', 'point token 0 === (21,12)');
chk(ptTokens[1] === '(23,14)', 'point token 1 === (23,14)');
chk(ptTokens[2] === '(25,16)', 'point token 2 === (25,16)');

// Line 2: sealed-class areas via exhaustive when, plus total = sum.
chk(lines[1].startsWith('areas='), 'line 2 starts with areas=');
chk(lines[1].indexOf('total=27') !== -1, 'line 2 contains substring total=27');
const areaPart = lines[1].slice('areas='.length).split(' total=')[0];
const areaTokens = areaPart.split(',').map(Number);
chk(areaTokens.length === 3, 'line 2 has 3 area values');
chk(areaTokens[0] === 12 && areaTokens[1] === 12 && areaTokens[2] === 3, 'area values === [12,12,3]');
const areaTotal = areaTokens.reduce((a, b) => a + b, 0);
chk(areaTotal === 27, 'sum of area values === 27 (matches total=)');
chk(lines[1] === 'areas=' + areaTokens.join(',') + ' total=' + areaTotal, 'line 2 reconstructs from tokens');

// Line 3: higher-order map/filter/sum over evens -> 2^2+4^2+6^2+8^2+10^2.
chk(lines[2].startsWith('evens^2 sum='), 'line 3 starts with evens^2 sum=');
const evensSum = [2, 4, 6, 8, 10].map(n => n * n).reduce((a, b) => a + b, 0);
chk(evensSum === 220, 'JS-computed evens^2 sum === 220');
chk(lines[2] === 'evens^2 sum=' + evensSum, 'line 3 equals computed evens^2 sum string');

// Line 4: generic extension fn secondOf(list) -> "b".
chk(lines[3] === 'second-of=' + ['a', 'b', 'c'][1], 'line 4 second-of equals list[1]=b');

// Line 5: recursion fib(15) -> 610. Compute fib independently in JS.
function fib(n) { let a = 0, b = 1; for (let i = 0; i < n; i++) { const t = a + b; a = b; b = t; } return a; }
chk(fib(15) === 610, 'JS-computed fib(15) === 610');
chk(lines[4] === 'fib(15)=' + fib(15), 'line 5 equals fib(15)=610 computed string');

// Line 6: null-safety chain resolves to YES.
chk(lines[5] === 'nullsafe=YES', 'line 6 nullsafe=YES (null-safety branch)');
chk(lines[5].split('=')[1] === 'YES', 'line 6 value token === YES');

// --- Section 7: whole-output structural invariants --------------------------
chk(outText.indexOf('undefined') === -1, 'output contains no literal undefined');
chk(outText.indexOf('=null') === -1, 'output contains no =null value token');
chk(outText.indexOf('NaN') === -1, 'output contains no NaN');
chk(outText.indexOf('Error') === -1, 'output contains no Error text');
const lfCount = (outText.match(/\n/g) || []).length;
chk(lfCount === 6, 'output has exactly 6 LF newlines');

// Determinism: a second run is byte-identical to the first.
const run2 = spawnSync(NODE, [APP], { cwd: DIR });
chk(run2.status === 0, 'second run exit 0');
chk(Buffer.compare(run2.stdout, outBuf) === 0, 'second run stdout byte-identical to first (deterministic)');
chk(run2.stderr.length === 0, 'second run stderr empty');

// --- Final tally ------------------------------------------------------------
console.log('KOTLINJS_RESULT ok=' + ok + ' fail=' + fail);
if (fail === 0) console.log('KOTLINJS_DONE');
process.exit(fail === 0 ? 0 : 1);
