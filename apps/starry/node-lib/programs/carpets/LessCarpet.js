'use strict';
/*
 * INDUSTRIAL carpet for less 4.2.2 (Node.js target v22.22.2).
 * Every assertion is an exact-value check: the EXACT expected compiled CSS
 * (or exact number/string/error field) is computed inline and compared with ===.
 * Golden values were observed by running this exact less@4.2.2 on node v22.22.2.
 *
 * Portability: every path is derived from __dirname; no hardcoded host paths,
 * no node binary path, no network, no Date.now/Math.random/wall-clock.
 */

const path = require('path');
const fs = require('fs');
const less = require('less');

let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }

// exact-string CSS equality assertion
function eq(name, actual, expected) {
  if (actual === expected) { ok++; }
  else {
    fail++;
    console.log('FAIL ' + name);
    console.log('  expected: ' + JSON.stringify(expected));
    console.log('  actual:   ' + JSON.stringify(actual));
  }
}

// Promise-form render -> resolves to css string
function css(str, opts) {
  return less.render(str, opts || {}).then(function (o) { return o.css; });
}

// Scratch dir derived from __dirname (NOT a hardcoded host path).
const TMP = path.join(__dirname, 'tmp-lesscarpet');

async function main() {
  fs.mkdirSync(TMP, { recursive: true });
  fs.writeFileSync(path.join(TMP, 'lib.less'), '@brand: #ff0000;\n.helper() { padding: 7px; }\n');
  fs.writeFileSync(path.join(TMP, 'raw.css'), 'body { color: green; }');

  // ---------------------------------------------------------------------------
  // API surface
  // ---------------------------------------------------------------------------
  chk(typeof less.render === 'function', 'api/render-is-function');
  chk(Array.isArray(less.version) && less.version[0] === 4 && less.version[1] === 2 && less.version[2] === 2, 'api/version-4.2.2');
  chk(typeof less.FileManager === 'function', 'api/FileManager');
  chk(typeof less.PluginLoader === 'function', 'api/PluginLoader');
  chk(typeof less.tree === 'object' && typeof less.tree.Dimension === 'function', 'api/tree.Dimension');
  chk(typeof less.visitors === 'object' && typeof less.visitors.Visitor === 'function', 'api/visitors.Visitor');
  chk(typeof less.functions === 'object', 'api/functions-registry');

  // render() returns an object with css/imports
  {
    const o = await less.render('@a: 5px; .x { width: @a + 1; }');
    eq('render/basic-arith-css', o.css, '.x {\n  width: 6px;\n}\n');
    chk(Array.isArray(o.imports) && o.imports.length === 0, 'render/imports-empty-array');
  }

  // Callback form (3-arg). Wrap in a promise so we can await it.
  {
    const cbCss = await new Promise(function (resolve, reject) {
      less.render('@a: 3px; .cb { margin: @a * 2; }', {}, function (err, out) {
        if (err) reject(err); else resolve(out.css);
      });
    });
    eq('render/callback-form', cbCss, '.cb {\n  margin: 6px;\n}\n');
  }

  // Explicit Promise (.then) form
  {
    const thenCss = await less.render('.p { x: 1; }').then(function (o) { return o.css; });
    eq('render/promise-then-form', thenCss, '.p {\n  x: 1;\n}\n');
  }

  // ---------------------------------------------------------------------------
  // Variables & interpolation
  // ---------------------------------------------------------------------------
  eq('var/selector-interp', await css('@name: foo; .@{name}-bar { color: red; }'),
    '.foo-bar {\n  color: red;\n}\n');
  eq('var/string-interp', await css('@base: 960; .x { content: "@{base}px"; }'),
    '.x {\n  content: "960px";\n}\n');
  eq('var/property-name-interp', await css('@my: banner; .@{my} { color: red; }'),
    '.banner {\n  color: red;\n}\n');
  eq('var/variable-variable', await css('@who: name; @name: red; .a { color: @@who; }'),
    '.a {\n  color: red;\n}\n');

  // ---------------------------------------------------------------------------
  // Arithmetic & operations
  // ---------------------------------------------------------------------------
  eq('arith/basic', await css('.x { width: 2px + 3px; height: 10px - 4px; margin: 4px*2; padding: 12px/4; }'),
    '.x {\n  width: 5px;\n  height: 6px;\n  margin: 8px;\n  padding: 12px/4;\n}\n');
  eq('arith/parens-mult', await css('.x { width: (1 + 1) * 2; }'),
    '.x {\n  width: 4;\n}\n');
  eq('arith/negative-var', await css('@a: 5px; .x { margin: -@a; }'),
    '.x {\n  margin: -5px;\n}\n');
  eq('arith/color-plus', await css('.x { color: #010203 + #040506; }'),
    '.x {\n  color: #050709;\n}\n');

  // ---------------------------------------------------------------------------
  // Math modes
  // ---------------------------------------------------------------------------
  eq('math/always', await css('.x { width: (2 + 3) * 4px; padding: 12px/4; }', { math: 'always' }),
    '.x {\n  width: 20px;\n  padding: 3px;\n}\n');
  eq('math/parens-division', await css('.x { width: (12px/4); height: 12px/4; }', { math: 'parens-division' }),
    '.x {\n  width: 3px;\n  height: 12px/4;\n}\n');

  // ---------------------------------------------------------------------------
  // Nesting & parent selector
  // ---------------------------------------------------------------------------
  eq('nest/amp', await css('.a { color: red; &:hover { color: blue; } .b & { color: green; } }'),
    '.a {\n  color: red;\n}\n.a:hover {\n  color: blue;\n}\n.b .a {\n  color: green;\n}\n');
  eq('nest/property-accessor', await css('.a { color: red; .b { background: $color; } }'),
    '.a {\n  color: red;\n}\n.a .b {\n  background: red;\n}\n');

  // ---------------------------------------------------------------------------
  // Mixins
  // ---------------------------------------------------------------------------
  eq('mixin/basic', await css('.mx() { color: red; } .a { .mx(); }'),
    '.a {\n  color: red;\n}\n');
  eq('mixin/param', await css('.mx(@c) { color: @c; } .a { .mx(blue); }'),
    '.a {\n  color: blue;\n}\n');
  eq('mixin/default-value', await css('.mx(@c: green) { color: @c; } .a { .mx(); }'),
    '.a {\n  color: green;\n}\n');
  eq('mixin/guard-iscolor', await css('.mx(@c) when (iscolor(@c)) { color: @c; } .a { .mx(red); }'),
    '.a {\n  color: red;\n}\n');
  eq('mixin/arguments', await css('.mx(...) { margin: @arguments; } .a { .mx(1px, 2px, 3px); }'),
    '.a {\n  margin: 1px 2px 3px;\n}\n');
  eq('mixin/returns-variables', await css('.m() { @w: 10px; } .a { .m(); width: @w; }'),
    '.a {\n  width: 10px;\n}\n');
  eq('mixin/important', await css('.mx() { color: red; } .a { .mx() !important; }'),
    '.a {\n  color: red !important;\n}\n');
  eq('mixin/namespace', await css('#ns { .mx(@c) { color: @c; } } .a { #ns.mx(red); }'),
    '.a {\n  color: red;\n}\n');
  eq('mixin/namespace-guarded', await css('#ns() { .mx() { color: blue; } } .a { #ns.mx(); }'),
    '.a {\n  color: blue;\n}\n');
  eq('mixin/recursive-loop', await css('.loop(@i) when (@i > 0) { .item-@{i} { width: (@i * 10px); } .loop(@i - 1); } .loop(3);'),
    '.item-3 {\n  width: 30px;\n}\n.item-2 {\n  width: 20px;\n}\n.item-1 {\n  width: 10px;\n}\n');

  // ---------------------------------------------------------------------------
  // Guards (conditional mixins)
  // ---------------------------------------------------------------------------
  eq('guard/default-fn', await css('.m(@a) when (@a > 0) { x: pos; } .m(@a) when (default()) { x: nonpos; } .y { .m(5); } .z { .m(-1); }'),
    '.y {\n  x: pos;\n}\n.z {\n  x: nonpos;\n}\n');
  eq('guard/and', await css('.m(@a) when (@a > 0) and (@a < 10) { x: mid; } .y { .m(5); }'),
    '.y {\n  x: mid;\n}\n');
  eq('guard/or-comma', await css('.m(@a) when (@a = 1), (@a = 2) { x: ok; } .y { .m(2); }'),
    '.y {\n  x: ok;\n}\n');
  eq('guard/not', await css('.m(@a) when not (@a = 0) { x: nz; } .y { .m(3); }'),
    '.y {\n  x: nz;\n}\n');
  eq('guard/default-vs-specific', await css('.m(@x) when (default()) { d: 1; } .m(1) { s: 1; } .a { .m(1); } .b { .m(2); }'),
    '.a {\n  s: 1;\n}\n.b {\n  d: 1;\n}\n');
  eq('guard/isnumber', await css('.m(@v) when (isnumber(@v)) { ok: yes; } .x { .m(42); }'),
    '.x {\n  ok: yes;\n}\n');

  // ---------------------------------------------------------------------------
  // extend (&:extend / :extend)
  // ---------------------------------------------------------------------------
  eq('extend/selector', await css('.a { color: red; } .b:extend(.a) {}'),
    '.a,\n.b {\n  color: red;\n}\n');
  eq('extend/inline', await css('.a { color: red; } .b { &:extend(.a); }'),
    '.a,\n.b {\n  color: red;\n}\n');
  eq('extend/all', await css('.a .x { color: red; } .b:extend(.x all) {}'),
    '.a .x,\n.a .b {\n  color: red;\n}\n');

  // ---------------------------------------------------------------------------
  // Detached rulesets, merge, maps
  // ---------------------------------------------------------------------------
  eq('detached/ruleset', await css('@dr: { color: red; }; .a { @dr(); }'),
    '.a {\n  color: red;\n}\n');
  eq('merge/comma', await css('.a { box-shadow+: inset 0 0 10px #555; box-shadow+: 0 0 20px black; }'),
    '.a {\n  box-shadow: inset 0 0 10px #555, 0 0 20px black;\n}\n');
  eq('merge/space', await css('.a { transform+_: scale(2); transform+_: rotate(15deg); }'),
    '.a {\n  transform: scale(2) rotate(15deg);\n}\n');
  eq('map/namespace-value', await css('@sizes: { small: 10px; large: 20px; }; .x { width: @sizes[small]; }'),
    '.x {\n  width: 10px;\n}\n');

  // ---------------------------------------------------------------------------
  // @media bubbling & nesting
  // ---------------------------------------------------------------------------
  eq('media/bubble', await css('.a { color: red; @media screen { color: blue; } }'),
    '.a {\n  color: red;\n}\n@media screen {\n  .a {\n    color: blue;\n  }\n}\n');
  eq('media/nested-merge', await css('@media screen { .a { @media (min-width: 768px) { color: red; } } }'),
    '@media screen and (min-width: 768px) {\n  .a {\n    color: red;\n  }\n}\n');
  eq('media/escaped-query-var', await css('@r: ~"(min-width: 768px)"; @media @r { .a { x: 1; } }'),
    '@media (min-width: 768px) {\n  .a {\n    x: 1;\n  }\n}\n');

  // ---------------------------------------------------------------------------
  // each()
  // ---------------------------------------------------------------------------
  eq('each/value', await css('@list: apple, pear; each(@list, { .sel-@{value} { x: @value; } });'),
    '.sel-apple {\n  x: apple;\n}\n.sel-pear {\n  x: pear;\n}\n');
  eq('each/index', await css('each(a b c, { .i-@{index} { v: @value; } });'),
    '.i-1 {\n  v: a;\n}\n.i-2 {\n  v: b;\n}\n.i-3 {\n  v: c;\n}\n');

  // ---------------------------------------------------------------------------
  // Comments & compression
  // ---------------------------------------------------------------------------
  eq('comments/block-kept-line-stripped', await css('/* keep */ .a { color: red; // line comment\n }'),
    '/* keep */\n.a {\n  color: red;\n}\n');
  eq('compress/basic', await css('.a { color: red;  margin: 0 0 0 0; }', { compress: true }),
    '.a{color:red;margin:0 0 0 0}');
  eq('compress/media', await css('.a { @media screen { color: red; } }', { compress: true }),
    '@media screen{.a{color:red}}');

  // ---------------------------------------------------------------------------
  // Escaping & string functions
  // ---------------------------------------------------------------------------
  eq('escape/tilde-calc', await css('.x { width: ~"calc(100% - 30px)"; }'),
    '.x {\n  width: calc(100% - 30px);\n}\n');
  eq('escape/e-quoted', await css('.x { font: ~"DejaVu Sans"; }'),
    '.x {\n  font: DejaVu Sans;\n}\n');
  eq('fn/e', await css('.x { content: e("hi"); }'),
    '.x {\n  content: hi;\n}\n');
  eq('fn/replace', await css('.x { content: replace("hello", "l", "L"); }'),
    '.x {\n  content: "heLlo";\n}\n');
  eq('fn/format-percent', await css('.x { content: %("rgb(%d, %d, %d)", 1, 2, 3); }'),
    '.x {\n  content: "rgb(1, 2, 3)";\n}\n');

  // ---------------------------------------------------------------------------
  // Numeric / math built-in functions
  // ---------------------------------------------------------------------------
  eq('fn/percentage', await css('.x { width: percentage(0.5); }'),
    '.x {\n  width: 50%;\n}\n');
  eq('fn/round', await css('.x { a: round(1.67); b: round(1.67, 1); }'),
    '.x {\n  a: 2;\n  b: 1.7;\n}\n');
  eq('fn/round-2dp', await css('.x { width: round(3.14159, 2); }'),
    '.x {\n  width: 3.14;\n}\n');
  eq('fn/ceil-floor', await css('.x { a: ceil(2.1); b: floor(2.9); }'),
    '.x {\n  a: 3;\n  b: 2;\n}\n');
  eq('fn/unit', await css('.x { a: unit(5, px); b: unit(5px); }'),
    '.x {\n  a: 5px;\n  b: 5;\n}\n');
  eq('fn/unit-replace', await css('.x { width: unit(100px, em); }'),
    '.x {\n  width: 100em;\n}\n');
  eq('fn/math-suite', await css('.x { a: sqrt(25); b: abs(-7); c: pow(3, 2); d: mod(8px, 3); e: min(3,1,2); f: max(3,1,2); }'),
    '.x {\n  a: 5;\n  b: 7;\n  c: 9;\n  d: 2px;\n  e: 1;\n  f: 3;\n}\n');
  eq('fn/pi', await css('.x { a: ceil(pi()); }'),
    '.x {\n  a: 4;\n}\n');
  eq('fn/convert', await css('.x { a: convert(9s, ms); b: convert(14cm, mm); }'),
    '.x {\n  a: 9000ms;\n  b: 140mm;\n}\n');
  eq('fn/range', await css('.a { content: range(3); }'),
    '.a {\n  content: 1 2 3;\n}\n');
  eq('fn/length', await css('.x { a: length(1 2 3 4); }'),
    '.x {\n  a: 4;\n}\n');
  eq('fn/length-extract', await css('@list: a b c; .x { len: length(@list); item: extract(@list, 2); }'),
    '.x {\n  len: 3;\n  item: b;\n}\n');

  // ---------------------------------------------------------------------------
  // Color functions
  // ---------------------------------------------------------------------------
  eq('color/lighten-darken', await css('.x { a: lighten(#888, 10%); b: darken(#888, 10%); }'),
    '.x {\n  a: #a2a2a2;\n  b: #6f6f6f;\n}\n');
  eq('color/mix', await css('.x { color: mix(#ff0000, #0000ff, 50%); }'),
    '.x {\n  color: #800080;\n}\n');
  eq('color/saturate-desaturate', await css('.x { a: saturate(#80b380, 20%); b: desaturate(#80b380, 20%); }'),
    '.x {\n  a: #6cc76c;\n  b: #949f94;\n}\n');
  eq('color/greyscale', await css('.x { color: greyscale(#80b380); }'),
    '.x {\n  color: #9a9a9a;\n}\n');
  eq('color/contrast', await css('.x { color: contrast(#000); bg: contrast(#fff); }'),
    '.x {\n  color: #ffffff;\n  bg: #000000;\n}\n');
  eq('color/fadein-fadeout', await css('.x { a: fadein(rgba(0,0,0,0.5), 10%); b: fadeout(rgba(0,0,0,0.5), 10%); }'),
    '.x {\n  a: rgba(0, 0, 0, 0.6);\n  b: rgba(0, 0, 0, 0.4);\n}\n');
  eq('color/spin-fade', await css('.x { a: spin(hsl(10,50%,50%), 30); b: fade(#000, 50%); }'),
    '.x {\n  a: hsl(40, 50%, 50%);\n  b: rgba(0, 0, 0, 0.5);\n}\n');
  eq('color/tint-shade', await css('.x { a: tint(#007fff, 50%); b: shade(#007fff, 50%); }'),
    '.x {\n  a: #80bfff;\n  b: #004080;\n}\n');
  eq('color/constructors', await css('.x { a: rgb(255,0,0); b: rgba(0,0,0,0.5); c: hsl(90, 100%, 50%); d: hsla(90,90%,50%,0.5); }'),
    '.x {\n  a: #ff0000;\n  b: rgba(0, 0, 0, 0.5);\n  c: hsl(90, 100%, 50%);\n  d: hsla(90, 90%, 50%, 0.5);\n}\n');
  eq('color/argb', await css('.x { color: argb(rgba(90,23,148,0.5)); }'),
    '.x {\n  color: #805a1794;\n}\n');
  eq('color/hsv', await css('.x { color: hsv(90, 100%, 50%); }'),
    '.x {\n  color: #408000;\n}\n');
  eq('color/parse-color-fn', await css('.x { color: color("#fdff01"); }'),
    '.x {\n  color: #fdff01;\n}\n');
  eq('color/components', await css('.x { a: red(#ff8000); b: green(#ff8000); c: blue(#ff8000); d: alpha(rgba(0,0,0,0.3)); e: hue(hsl(98, 12%, 95%)); f: saturation(hsl(98,12%,95%)); g: lightness(hsl(98,12%,95%)); }'),
    '.x {\n  a: 255;\n  b: 128;\n  c: 0;\n  d: 0.3;\n  e: 98;\n  f: 12%;\n  g: 95%;\n}\n');

  // ---------------------------------------------------------------------------
  // strictUnits
  // ---------------------------------------------------------------------------
  eq('strictUnits/off-mixes', await css('.x { width: 2px + 3; }'),
    '.x {\n  width: 5px;\n}\n');
  eq('strictUnits/on-converts-length', await css('.x { width: 2px + 3cm; }', { strictUnits: true }),
    '.x {\n  width: 115.38582677px;\n}\n');

  // ---------------------------------------------------------------------------
  // @import via paths (write helper .less under __dirname-derived TMP)
  // ---------------------------------------------------------------------------
  {
    const o = await less.render('@import "lib"; .x { color: @brand; .helper(); }', { paths: [TMP] });
    eq('import/paths-css', o.css, '.x {\n  color: #ff0000;\n  padding: 7px;\n}\n');
    chk(o.imports.length === 1, 'import/imports-count');
    chk(path.isAbsolute(o.imports[0]) && o.imports[0].endsWith('lib.less'), 'import/imports-path-endswith');
  }
  eq('import/reference', await css('@import (reference) "lib"; .x { color: @brand; }', { paths: [TMP] }),
    '.x {\n  color: #ff0000;\n}\n');
  eq('import/inline', await css('@import (inline) "raw.css"; .x { color: red; }', { paths: [TMP] }),
    'body { color: green; }\n.x {\n  color: red;\n}\n');
  eq('import/optional-missing', await css('@import (optional) "nope.less"; .x { color: red; }', { paths: [TMP] }),
    '.x {\n  color: red;\n}\n');
  eq('import/css-passthrough', await css('@import "raw.css"; @import "raw.css";', { paths: [TMP] }),
    '@import "raw.css";\n@import "raw.css";\n');

  // ---------------------------------------------------------------------------
  // Plugins
  // ---------------------------------------------------------------------------
  // (1) function-adding plugin
  {
    const plugin = {
      install: function (lessLocal, pluginManager, functions) {
        functions.add('triple', function (v) {
          return new lessLocal.tree.Dimension(v.value * 3, v.unit);
        });
      }
    };
    eq('plugin/function-add', await css('.x { width: triple(4px); }', { plugins: [plugin] }),
      '.x {\n  width: 12px;\n}\n');
  }
  // (2) visitor plugin: rename declaration 'foo' -> 'bar'
  {
    const Visitor = less.visitors.Visitor;
    function MyVisitor() { this._visitor = new Visitor(this); }
    MyVisitor.prototype = {
      isReplacing: false,
      run: function (root) { return this._visitor.visit(root); },
      visitDeclaration: function (node) { if (node.name === 'foo') { node.name = 'bar'; } return node; }
    };
    const plugin = { install: function (l, pm) { pm.addVisitor(new MyVisitor()); } };
    eq('plugin/visitor-rename', await css('.x { foo: 10px; baz: 1; }', { plugins: [plugin] }),
      '.x {\n  bar: 10px;\n  baz: 1;\n}\n');
  }
  // (3) custom FileManager serving a virtual @import target
  {
    function VirtualPlugin(lessLocal) {
      const FM = function () {};
      FM.prototype = new lessLocal.FileManager();
      FM.prototype.supports = function (filename) { return filename.indexOf('virtual:') === 0; };
      FM.prototype.supportsSync = function () { return false; };
      FM.prototype.loadFile = function (filename) {
        const m = { 'virtual:colors': '@primary: #123456; .pmix() { border: 1px solid @primary; }' };
        return Promise.resolve({ contents: m[filename] || '', filename: filename });
      };
      return { install: function (l, pm) { pm.addFileManager(new FM()); } };
    }
    eq('plugin/file-manager-virtual',
      await css('@import "virtual:colors"; .x { color: @primary; .pmix(); }', { plugins: [VirtualPlugin(less)] }),
      '.x {\n  color: #123456;\n  border: 1px solid #123456;\n}\n');
  }

  // ---------------------------------------------------------------------------
  // Error paths (LessError with line info)
  // ---------------------------------------------------------------------------
  // Parse error
  {
    let caught = null;
    try { await less.render('.a { color: red'); } catch (e) { caught = e; }
    chk(caught !== null, 'error/parse-thrown');
    chk(caught && caught.constructor && caught.constructor.name === 'LessError', 'error/parse-is-LessError');
    chk(caught instanceof Error, 'error/parse-instanceof-Error');
    chk(caught && caught.type === 'Parse', 'error/parse-type');
    chk(caught && caught.line === 1, 'error/parse-line');
    chk(caught && caught.column === 15, 'error/parse-column');
    chk(caught && caught.message === 'Unrecognised input. Possibly missing something', 'error/parse-message');
    chk(caught && caught.toString().indexOf('ParseError:') === 0, 'error/parse-toString-prefix');
  }
  // Name error (undefined variable)
  {
    let caught = null;
    try { await less.render('.x { color: @nope; }'); } catch (e) { caught = e; }
    chk(caught && caught.type === 'Name', 'error/name-type');
    chk(caught && caught.line === 1, 'error/name-line');
    chk(caught && caught.column === 12, 'error/name-column');
    chk(caught && caught.message === 'variable @nope is undefined', 'error/name-message');
  }
  // Syntax error (strictUnits multiplication)
  {
    let caught = null;
    try { await less.render('.x { width: 2px * 3px; }', { strictUnits: true }); } catch (e) { caught = e; }
    chk(caught && caught.type === 'Syntax', 'error/syntax-type');
    chk(caught && caught.message === 'Multiple units in dimension. Correct the units or use the unit function. Bad unit: px*px', 'error/syntax-message');
  }

  // cleanup scratch dir
  fs.rmSync(TMP, { recursive: true, force: true });
}

main().then(function () {
  console.log('LESS_RESULT ok=' + ok + ' fail=' + fail);
  if (fail === 0) console.log('LESS_DONE');
  process.exit(fail === 0 ? 0 : 1);
}).catch(function (e) {
  console.log('CARPET_CRASH ' + (e && e.stack ? e.stack : e));
  try { fs.rmSync(TMP, { recursive: true, force: true }); } catch (x) {}
  console.log('LESS_RESULT ok=' + ok + ' fail=' + (fail + 1));
  process.exit(1);
});
