'use strict';
/*
 * INDUSTRIAL carpet for stylus 0.64.0
 * Target runtime: Node.js v22.22.2 (StarryOS target). Host-verified on the same.
 * Deterministic only: no Date.now / Math.random / timestamps / wall-clock / network.
 * Every assertion is an exact-value check (=== against a golden baked from observed
 * real output of this exact stylus version).
 * Portability: stylus resolved via require() from cwd node_modules; every scratch path
 * derived from __dirname; subprocess (none needed) would use process.execPath.
 */

const path = require('path');
const fs = require('fs');
const stylus = require('stylus');

let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }
function eq(actual, expected, name) {
  if (actual === expected) ok++;
  else { fail++; console.log('FAIL ' + name + ' :: got ' + JSON.stringify(actual) + ' want ' + JSON.stringify(expected)); }
}

// Synchronous render helper (stylus invokes the callback synchronously).
function rcb(src, opts) {
  let out, errOut;
  const r = stylus(src, opts || {});
  r.render(function (e, c) { if (e) errOut = e; else out = c; });
  if (errOut) throw errOut;
  return out;
}

// Scratch dir derived from __dirname (NOT a hardcoded absolute host path).
const SCRATCH = path.join(__dirname, 'tmp-styluscarpet');
function withFile(rel, content, body) {
  fs.mkdirSync(SCRATCH, { recursive: true });
  const f = path.join(SCRATCH, rel);
  fs.writeFileSync(f, content);
  try { return body(f, SCRATCH); }
  finally { try { fs.rmSync(SCRATCH, { recursive: true, force: true }); } catch (_) {} }
}

// ---------------------------------------------------------------------------
// Group A: module / API surface
// ---------------------------------------------------------------------------
eq(stylus.version, '0.64.0', 'version');
chk(typeof stylus === 'function', 'stylus-is-function');
chk(typeof stylus.render === 'function', 'render-is-function');
chk(typeof stylus.convertCSS === 'function', 'convertCSS-is-function');
chk(typeof stylus.Parser === 'function', 'Parser-exposed');
chk(typeof stylus.Evaluator === 'function', 'Evaluator-exposed');
chk(typeof stylus.Compiler === 'function', 'Compiler-exposed');
chk(typeof stylus.Normalizer === 'function', 'Normalizer-exposed');
chk(typeof stylus.nodes === 'object' && typeof stylus.nodes.Unit === 'function', 'nodes-Unit-exposed');
chk(typeof stylus.functions === 'object', 'functions-exposed');
chk(typeof stylus.utils === 'object', 'utils-exposed');

// ---------------------------------------------------------------------------
// Group B: render sync + async + convertCSS
// ---------------------------------------------------------------------------
eq(stylus.render('body\n  color red\n'), 'body {\n  color: #f00;\n}\n', 'render-sync-basic');
eq(rcb('a\n  color red\n'), 'a {\n  color: #f00;\n}\n', 'render-async-cb');
eq(stylus.convertCSS('body { color: red; }'), 'body\n  color: red\n\n', 'convertCSS');

// ---------------------------------------------------------------------------
// Group C: variables + assignment
// ---------------------------------------------------------------------------
eq(stylus.render('c = #f00\nbody\n  color c\n'), 'body {\n  color: #f00;\n}\n', 'variable-assign');

// ---------------------------------------------------------------------------
// Group D: arithmetic, units, precedence, operators
// ---------------------------------------------------------------------------
eq(stylus.render('body\n  width 10px + 5px\n  margin (10px / 2)\n  z 2 * 3\n'),
   'body {\n  width: 15px;\n  margin: 5px;\n  z: 6;\n}\n', 'arithmetic-units');
eq(stylus.render('body\n  width (1 + 2) * 3px\n'), 'body {\n  width: 9px;\n}\n', 'paren-precedence');
eq(stylus.render('body\n  z 1s + 500ms\n'), 'body {\n  z: 1.5s;\n}\n', 'unit-coercion');
eq(stylus.render('body\n  width 50% - 10px\n'), 'body {\n  width: 40%;\n}\n', 'percent-minus');
eq(stylus.render('body\n  z 10 % 3\n'), 'body {\n  z: 1;\n}\n', 'modulo');
eq(stylus.render('body\n  z 2 ** 3\n'), 'body {\n  z: 8;\n}\n', 'power');
eq(stylus.render('body\n  z 16 ** 0.5\n'), 'body {\n  z: 4;\n}\n', 'power-sqrt');

// ---------------------------------------------------------------------------
// Group E: color operations + built-in color functions
// ---------------------------------------------------------------------------
eq(stylus.render('body\n  color rgba(255,0,0,0.5)\n'), 'body {\n  color: rgba(255,0,0,0.5);\n}\n', 'rgba-literal');
eq(stylus.render('body\n  color rgb(255,0,0)\n'), 'body {\n  color: #f00;\n}\n', 'rgb');
eq(stylus.render('body\n  color lighten(#000, 50%)\n'), 'body {\n  color: #808080;\n}\n', 'lighten');
eq(stylus.render('body\n  color darken(#fff, 50%)\n'), 'body {\n  color: #808080;\n}\n', 'darken');
eq(stylus.render('body\n  color mix(#fff, #000, 50%)\n'), 'body {\n  color: #7f7f7f;\n}\n', 'mix');
eq(stylus.render('body\n  color invert(#fff)\n'), 'body {\n  color: #000;\n}\n', 'invert');
eq(stylus.render('body\n  color #001122 + #110000\n'), 'body {\n  color: #112;\n}\n', 'color-add');
eq(stylus.render('body\n  color rgba(#123456, 0.3)\n'), 'body {\n  color: rgba(18,52,86,0.3);\n}\n', 'rgba-from-hex');
eq(stylus.render('c = rgba(10,20,30,0.4)\nbody\n  r red(c)\n  g green(c)\n  b blue(c)\n  a alpha(c)\n'),
   'body {\n  r: 10;\n  g: 20;\n  b: 30;\n  a: 0.4;\n}\n', 'color-components');
// NB: property name avoids the `s` built-in (a bare `s <expr>` parses as a call to s()).
eq(stylus.render('c = #f00\nbody\n  h hue(c)\n  sat saturation(c)\n  l lightness(c)\n'),
   'body {\n  h: 0deg;\n  sat: 100%;\n  l: 50%;\n}\n', 'hsl-components');
eq(stylus.render('body\n  l luminosity(#fff)\n'), 'body {\n  l: 1;\n}\n', 'luminosity');
eq(stylus.render('body\n  color transparentify(#808080, #000, 0.5)\n'),
   'body {\n  color: rgba(255,255,255,0.5);\n}\n', 'transparentify');
eq(stylus.render('body\n  z component(#abc, \'red\')\n'), 'body {\n  z: 170;\n}\n', 'component');
eq(stylus.render('body\n  color hsla(0, 100%, 50%, 0.5)\n'), 'body {\n  color: rgba(255,0,0,0.5);\n}\n', 'hsla');
eq(stylus.render('body\n  color hsl(120, 50%, 50%)\n'), 'body {\n  color: #40bf40;\n}\n', 'hsl');

// ---------------------------------------------------------------------------
// Group F: nesting, &, selector references
// ---------------------------------------------------------------------------
eq(stylus.render('.a\n  .b\n    color red\n'), '.a .b {\n  color: #f00;\n}\n', 'nesting');
eq(stylus.render('a\n  &:hover\n    color blue\n'), 'a:hover {\n  color: #00f;\n}\n', 'amp-parent-ref');
eq(stylus.render('a, b\n  &:hover\n    color red\n'), 'a:hover,\nb:hover {\n  color: #f00;\n}\n', 'amp-multi-parent');
eq(stylus.render('.foo\n  .bar\n    width 1px\n    ^[0]:hover &\n      width 2px\n'),
   '.foo .bar {\n  width: 1px;\n}\n.foo:hover .foo .bar {\n  width: 2px;\n}\n', 'partial-ref-N');
eq(stylus.render('.a\n  .b\n    .c\n      ^[-1]:hover &\n        color red\n'),
   '.a .b:hover .a .b .c {\n  color: #f00;\n}\n', 'partial-ref-neg');
eq(stylus.render('.foo\n  content selector()\n'), '.foo {\n  content: \'.foo\';\n}\n', 'selector-builtin');

// ---------------------------------------------------------------------------
// Group G: mixins & functions
// ---------------------------------------------------------------------------
eq(stylus.render('tabs()\n  display block\n.tabs\n  tabs()\n'), '.tabs {\n  display: block;\n}\n', 'mixin-call');
eq(stylus.render('foo()\n  .bar\n    {block}\n+foo()\n  color red\n'), '.bar {\n  color: #f00;\n}\n', 'block-mixin');
eq(stylus.render('add(a, b)\n  a + b\nbody\n  width add(1px, 2px)\n'), 'body {\n  width: 3px;\n}\n', 'fn-return-value');
eq(stylus.render('box(w = 10px, h = 20px)\n  width w\n  height h\n.b\n  box()\n'),
   '.b {\n  width: 10px;\n  height: 20px;\n}\n', 'fn-default-args');
eq(stylus.render('m(args...)\n  margin args\n.r\n  m(1px 2px 3px)\n'), '.r {\n  margin: 1px 2px 3px;\n}\n', 'fn-rest-args');
eq(stylus.render('box(w, h)\n  width w\n  height h\n.n\n  box(h: 5px, w: 3px)\n'),
   '.n {\n  width: 3px;\n  height: 5px;\n}\n', 'fn-named-args');

// use() plugin defining a JS function
eq(rcb('body\n  width double(4px)\n', { use: function (r) {
     r.define('double', function (n) { return new stylus.nodes.Unit(n.val * 2, n.type); });
   } }), 'body {\n  width: 8px;\n}\n', 'use-plugin-option');
(function () {
  let css;
  const plugin = function (renderer) {
    renderer.define('triple', function (n) { return new stylus.nodes.Unit(n.val * 3, n.type); });
  };
  stylus('body\n  width triple(3px)\n').use(plugin).render(function (e, c) { if (e) throw e; css = c; });
  eq(css, 'body {\n  width: 9px;\n}\n', 'use-plugin-method');
})();

// ---------------------------------------------------------------------------
// Group H: interpolation
// ---------------------------------------------------------------------------
eq(stylus.render("v = 10\nbody\n  margin-{'top'} 5px\n"), 'body {\n  margin-top: 5px;\n}\n', 'interpolation');

// ---------------------------------------------------------------------------
// Group I: conditionals
// ---------------------------------------------------------------------------
eq(stylus.render('n = 5\nbody\n  if n > 3\n    color red\n  else\n    color blue\n'),
   'body {\n  color: #f00;\n}\n', 'if-else');
eq(stylus.render('n = 2\nbody\n  if n > 3\n    color red\n  else if n > 1\n    color green\n  else\n    color blue\n'),
   'body {\n  color: #008000;\n}\n', 'if-elseif-else');
eq(stylus.render('n = 1\nbody\n  unless n is 0\n    color green\n'), 'body {\n  color: #008000;\n}\n', 'unless');
eq(stylus.render('n = 1\nbody\n  color (n > 0 ? red : blue)\n'), 'body {\n  color: #f00;\n}\n', 'ternary');
eq(stylus.render('n = 1\nbody\n  color red if n > 0\n'), 'body {\n  color: #f00;\n}\n', 'postfix-if');
eq(stylus.render('body\n  color (true and 1 ? red : blue)\n'), 'body {\n  color: #f00;\n}\n', 'logical-and');

// ---------------------------------------------------------------------------
// Group J: iteration
// ---------------------------------------------------------------------------
eq(stylus.render('for i in 1..3\n  .col-{i}\n    width i\n'),
   '.col-1 {\n  width: 1;\n}\n.col-2 {\n  width: 2;\n}\n.col-3 {\n  width: 3;\n}\n', 'for-range-inclusive');
eq(stylus.render('for i in 1...4\n  .e{i}\n    x i\n'),
   '.e1 {\n  x: 1;\n}\n.e2 {\n  x: 2;\n}\n.e3 {\n  x: 3;\n}\n', 'for-range-exclusive');
eq(stylus.render('for v, i in a b c\n  .item-{i}\n    content v\n'),
   '.item-0 {\n  content: a;\n}\n.item-1 {\n  content: b;\n}\n.item-2 {\n  content: c;\n}\n', 'for-value-index');

// ---------------------------------------------------------------------------
// Group K: hashes / objects
// ---------------------------------------------------------------------------
eq(stylus.render("h = { foo: 1px, bar: 2px }\nbody\n  width h.foo\n  height h['bar']\n"),
   'body {\n  width: 1px;\n  height: 2px;\n}\n', 'hash-access');
eq(stylus.render('h = { a: 1px, b: 2px }\nfor key, val in h\n  .{key}\n    width val\n'),
   '.a {\n  width: 1px;\n}\n.b {\n  width: 2px;\n}\n', 'hash-iteration');

// ---------------------------------------------------------------------------
// Group L: @extend
// ---------------------------------------------------------------------------
eq(stylus.render('.msg\n  padding 10px\n.err\n  @extend .msg\n  color red\n'),
   '.msg,\n.err {\n  padding: 10px;\n}\n.err {\n  color: #f00;\n}\n', 'extend');
eq(stylus.render('$base\n  margin 0\n.x\n  @extend $base\n  color red\n'),
   '.x {\n  margin: 0;\n}\n.x {\n  color: #f00;\n}\n', 'extend-placeholder');

// ---------------------------------------------------------------------------
// Group M: built-in functions (type/math/list/string)
// ---------------------------------------------------------------------------
eq(stylus.render('body\n  z typeof(123)\n'), "body {\n  z: 'unit';\n}\n", 'typeof-unit');
eq(stylus.render("body\n  a typeof('s')\n  b typeof(1px)\n  c typeof(#fff)\n  d typeof(true)\n"),
   "body {\n  a: 'string';\n  b: 'unit';\n  c: 'rgba';\n  d: 'boolean';\n}\n", 'typeof-variants');
eq(stylus.render('body\n  z type(#fff)\n'), "body {\n  z: 'rgba';\n}\n", 'type-alias');
eq(stylus.render('body\n  a abs(-5)\n  b round(4.6)\n  c ceil(4.1)\n'),
   'body {\n  a: 5;\n  b: 5;\n  c: 5;\n}\n', 'abs-round-ceil');
eq(stylus.render('body\n  z floor(4.9)\n'), 'body {\n  z: 4;\n}\n', 'floor');
eq(stylus.render('body\n  z abs(-5px)\n'), 'body {\n  z: 5px;\n}\n', 'abs-unit-preserve');
eq(stylus.render('body\n  z round(3.14159, 2)\n'), 'body {\n  z: 3.14;\n}\n', 'round-precision');
eq(stylus.render('body\n  a max(3, 7)\n  b min(3, 7)\n'), 'body {\n  a: 7;\n  b: 3;\n}\n', 'min-max');
eq(stylus.render('body\n  z percentage(0.5)\n'), 'body {\n  z: 50%;\n}\n', 'percentage');
eq(stylus.render('body\n  z base-convert(255, 16, 2)\n'), 'body {\n  z: ff;\n}\n', 'base-convert');
eq(stylus.render('body\n  width unit(10, px)\n'), 'body {\n  width: 10px;\n}\n', 'unit-set');
eq(stylus.render("body\n  z unit(15px, '')\n"), 'body {\n  z: 15;\n}\n', 'unit-strip');
eq(stylus.render('list = 1 2 3\nbody\n  z length(list)\n'), 'body {\n  z: 3;\n}\n', 'length');
eq(stylus.render('body\n  z length(())\n'), 'body {\n  z: 0;\n}\n', 'length-empty');
eq(stylus.render('body\n  z length(5px)\n'), 'body {\n  z: 1;\n}\n', 'length-single');
eq(stylus.render('list = 1 2\npush(list, 3)\nbody\n  z length(list)\n'), 'body {\n  z: 3;\n}\n', 'push');
eq(stylus.render('l = 1 2 3\nx = pop(l)\nbody\n  a x\n  b length(l)\n'), 'body {\n  a: 3;\n  b: 2;\n}\n', 'pop');
eq(stylus.render('l = 1 2 3\nx = shift(l)\nbody\n  a x\n  b length(l)\n'), 'body {\n  a: 1;\n  b: 2;\n}\n', 'shift');
eq(stylus.render('l = 2 3\nunshift(l, 1)\nbody\n  z length(l)\n'), 'body {\n  z: 3;\n}\n', 'unshift');
eq(stylus.render('body\n  z length(slice(1 2 3 4, 1, 3))\n'), 'body {\n  z: 2;\n}\n', 'slice');
eq(stylus.render("body\n  z match('^foo', 'foobar')\n"), "body {\n  z: 'foo';\n}\n", 'match');
eq(stylus.render("body\n  content replace('a', 'b', 'aaa')\n"), "body {\n  content: 'bbb';\n}\n", 'replace');
eq(stylus.render("body\n  content substr('hello', 1, 3)\n"), "body {\n  content: 'ell';\n}\n", 'substr');
eq(stylus.render("body\n  z length(split(',', 'a,b,c'))\n"), 'body {\n  z: 3;\n}\n', 'split');
eq(stylus.render("body\n  z operate('+', 5, 3)\n"), 'body {\n  z: 8;\n}\n', 'operate');
eq(stylus.render('body\n  width s("%s + %s", 1px, 2px)\n'), 'body {\n  width: 1px + 2px;\n}\n', 's-format');
eq(stylus.render("body\n  content unquote('\"hi\"')\n"), 'body {\n  content: "hi";\n}\n', 'unquote');

// p() built-in inspects to stdout and returns null -> property renders empty.
// Suppress its console.log so it does not pollute carpet output, but assert behaviour.
(function () {
  const orig = console.log;
  let printed = '';
  console.log = function () { printed += Array.prototype.join.call(arguments, ' '); };
  let css;
  try { css = stylus.render('body\n  width p(10px)\n'); } finally { console.log = orig; }
  eq(css, 'body {\n  width: ;\n}\n', 'p-returns-null');
  chk(/inspect/.test(printed) && /10px/.test(printed), 'p-inspects-stdout');
})();

// ---------------------------------------------------------------------------
// Group N: define() / set() / get() JS API + define/globals/functions options
// ---------------------------------------------------------------------------
(function () {
  let css;
  stylus('body\n  width foo\n').define('foo', new stylus.nodes.Unit(42, 'px'))
    .render(function (e, c) { if (e) throw e; css = c; });
  eq(css, 'body {\n  width: 42px;\n}\n', 'define-global-node');
})();
(function () {
  let css;
  stylus('body\n  width add(2, 3)\n').define('add', function (a, b) { return new stylus.nodes.Unit(a.val + b.val); })
    .render(function (e, c) { if (e) throw e; css = c; });
  eq(css, 'body {\n  width: 5;\n}\n', 'define-js-function');
})();
(function () {
  let css;
  stylus('body\n  font family\n').define('family', 'Arial')
    .render(function (e, c) { if (e) throw e; css = c; });
  eq(css, "body {\n  font: 'Arial';\n}\n", 'define-coerce-string');
})();
(function () {
  const r = stylus('body\n  color red\n').set('compress', true);
  chk(r.get('compress') === true, 'set-get');
  let css; r.render(function (e, c) { if (e) throw e; css = c; });
  eq(css, 'body{color:#f00}', 'set-compress-renders');
})();
eq(stylus.render('body\n  width gw\n', { globals: { gw: new stylus.nodes.Unit(9, 'px') } }),
   'body {\n  width: 9px;\n}\n', 'option-globals');
eq(stylus.render('body\n  width triple(2)\n',
   { functions: { triple: function (n) { return new stylus.nodes.Unit(n.val * 3, n.type); } } }),
   'body {\n  width: 6;\n}\n', 'option-functions');

// ---------------------------------------------------------------------------
// Group O: imports / paths / include / import() / json / deps
// ---------------------------------------------------------------------------
eq(withFile('vars.styl', 'primary = #abc\n', function (f) {
     return stylus.render('body\n  color primary\n', { imports: [f] });
   }), 'body {\n  color: #abc;\n}\n', 'option-imports');
eq(withFile('mod.styl', '.mod\n  color red\n', function (f, dir) {
     return stylus.render("@import 'mod'\n", { paths: [dir] });
   }), '.mod {\n  color: #f00;\n}\n', 'import-via-paths');
eq(withFile('lib.styl', '.lib\n  color blue\n', function (f, dir) {
     let css;
     stylus("@import 'lib'\n").include(dir).render(function (e, c) { if (e) throw e; css = c; });
     return css;
   }), '.lib {\n  color: #00f;\n}\n', 'include-method');
eq(withFile('g.styl', 'gvar = #0f0\n', function (f, dir) {
     let css;
     stylus('body\n  color gvar\n', { paths: [dir] }).import('g').render(function (e, c) { if (e) throw e; css = c; });
     return css;
   }), 'body {\n  color: #0f0;\n}\n', 'import-method');
eq(withFile('v.json', JSON.stringify({ width: '10px', nested: { color: '#abc' } }), function (f, dir) {
     return stylus.render("vars = json('v.json', { hash: true })\nbody\n  width vars.width\n  color vars.nested.color\n", { paths: [dir] });
   }), 'body {\n  width: 10px;\n  color: #abc;\n}\n', 'json-hash-file');
eq(withFile('c.json', JSON.stringify({ 'primary-color': '#f00' }), function (f, dir) {
     return stylus.render("json('c.json')\nbody\n  color primary-color\n", { paths: [dir] });
   }), 'body {\n  color: #f00;\n}\n', 'json-vars-file');
eq(withFile('a.styl', '.a\n  color red\n', function (f, dir) {
     const r = stylus("@import 'a'\n", { paths: [dir] });
     return r.deps().map(function (p) { return path.basename(p); }).join(',');
   }), 'a.styl', 'deps');

// ---------------------------------------------------------------------------
// Group P: @media, @keyframes, !important, preserved comments
// ---------------------------------------------------------------------------
eq(stylus.render('@media (max-width: 600px)\n  body\n    color red\n'),
   '@media (max-width: 600px) {\n  body {\n    color: #f00;\n  }\n}\n', 'media');
(function () {
  const block = function (pfx) {
    return '@' + pfx + 'keyframes foo {\n  0% {\n    opacity: 0;\n  }\n  100% {\n    opacity: 1;\n  }\n}\n';
  };
  const expected = block('-moz-') + block('-webkit-') + block('-o-') + block('');
  eq(stylus.render('@keyframes foo\n  0%\n    opacity 0\n  100%\n    opacity 1\n'), expected, 'keyframes-prefixed');
})();
eq(stylus.render('body\n  color red !important\n'), 'body {\n  color: #f00 !important;\n}\n', 'important');
eq(stylus.render('/*!\n * keep\n */\nbody\n  color red\n'),
   '/*\n * keep\n */\nbody {\n  color: #f00;\n}\n', 'preserved-comment');

// ---------------------------------------------------------------------------
// Group Q: compress option
// ---------------------------------------------------------------------------
eq(stylus.render('body\n  color red\n', { compress: true }), 'body{color:#f00}', 'compress');
eq(stylus.render('.a\n  .b\n    color red\n  margin 0\n', { compress: true }),
   '.a{margin:0}.a .b{color:#f00}', 'compress-nested');

// ---------------------------------------------------------------------------
// Group R: ParseError paths (sync throw + async err callback + error shape)
// ---------------------------------------------------------------------------
(function () {
  let threw = false, name = '';
  try { stylus.render('body\n  color (\n'); }
  catch (e) { threw = true; name = e.name; }
  chk(threw && name === 'ParseError', 'parse-error-sync-throw');
})();
(function () {
  let errName = 'NONE';
  stylus('body\n  color (\n').render(function (e, c) { if (e) errName = e.name; });
  eq(errName, 'ParseError', 'parse-error-async-cb');
})();
(function () {
  let okShape = false;
  try { stylus.render('@@\n'); }
  catch (e) { okShape = e.name === 'ParseError' && (/expected/i.test(e.message) || /unexpected/i.test(e.message)); }
  chk(okShape, 'parse-error-message');
})();

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------
console.log('STYLUS_RESULT ok=' + ok + ' fail=' + fail);
if (fail === 0) console.log('STYLUS_DONE');
process.exit(fail === 0 ? 0 : 1);
