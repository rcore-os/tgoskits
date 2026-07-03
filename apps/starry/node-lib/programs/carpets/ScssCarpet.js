'use strict';
/*
 * INDUSTRIAL CARPET: dart-sass (sass) 1.83.4 — Dart Sass JS, SCSS syntax.
 * Runtime: Node.js v22.22.2 (StarryOS target). Deterministic, no clock/random/network.
 * Every assertion is an exact-value (===) check against a genuine golden observed on this
 * exact sass version (1.83.4 / dart2js 3.6.1).
 *
 * Self-check protocol:
 *   let ok=0, fail=0; chk(cond,name)
 *   final: console.log('SCSS_RESULT ok='+ok+' fail='+fail); if(fail===0) console.log('SCSS_DONE'); process.exit(fail===0?0:1)
 *
 * Portability: all paths derive from __dirname; subprocess (none needed) would use process.execPath.
 */

const sass = require('sass');               // NOTE: require('sass/package.json') is blocked by exports; require('sass') works.
const fs = require('fs');
const path = require('path');

const SILENT = sass.Logger.silent;
let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }

// Compile SCSS string with a silent logger so deprecation chatter never pollutes stdout.
function C(scss, opts) {
  return sass.compileString(scss, Object.assign({ logger: SILENT }, opts || {})).css;
}

// Scratch dir derived from __dirname (portable, cleaned up at the end).
const TMP = path.join(__dirname, 'tmp-scsscarpet');

async function main() {
  // ---------- 1. API surface ----------
  chk(sass.info.split('\t')[0] === 'dart-sass', 'info_impl');
  chk(sass.info.split('\t')[1] === '1.83.4', 'info_version');
  chk(typeof sass.compile === 'function', 'api_compile');
  chk(typeof sass.compileString === 'function', 'api_compileString');
  chk(typeof sass.compileAsync === 'function', 'api_compileAsync');
  chk(typeof sass.compileStringAsync === 'function', 'api_compileStringAsync');
  chk(typeof sass.render === 'function', 'api_render');
  chk(typeof sass.renderSync === 'function', 'api_renderSync');
  chk(typeof sass.initCompiler === 'function', 'api_initCompiler');
  chk(typeof sass.initAsyncCompiler === 'function', 'api_initAsyncCompiler');
  chk(typeof sass.Exception === 'function', 'api_Exception');
  chk(Object.keys(sass.types).sort().join(',') === 'Boolean,Color,Error,List,Map,Null,Number,String', 'api_types');

  // ---------- 2. Basic compileString / output styles ----------
  chk(C('.a{ color: red; }') === '.a {\n  color: red;\n}', 'basic_expanded');
  chk(C('.a{ color: red; }', { style: 'expanded' }) === '.a {\n  color: red;\n}', 'style_expanded_explicit');
  const compressed = C('.a{ color: red; background: white; }', { style: 'compressed' });
  chk(compressed === '.a{color:red;background:#fff}', 'style_compressed_value');
  chk(compressed.indexOf('\n') === -1, 'style_compressed_no_newline');
  chk(C('.a{c:red} .b{c:blue}') === '.a {\n  c: red;\n}\n\n.b {\n  c: blue;\n}', 'two_rules_spacing');

  // ---------- 3. Variables / !default / !global ----------
  chk(C('$c: blue !default;\n.a{ color:$c; }') === '.a {\n  color: blue;\n}', 'var_default_unset');
  chk(C('$c: blue;\n$c: green !default;\n.a{ color:$c; }') === '.a {\n  color: blue;\n}', 'var_default_kept');
  chk(C('$x: 1;\n@mixin m(){ $x: 9 !global; }\n@include m();\n.a{ v: $x; }') === '.a {\n  v: 9;\n}', 'var_global');

  // ---------- 4. Nesting + parent selector & ----------
  chk(C('.btn{ &:hover{ color:red; } &.active{ color:blue; } }')
      === '.btn:hover {\n  color: red;\n}\n.btn.active {\n  color: blue;\n}', 'nesting_amp');
  chk(C('.a{ color: red; .b{ width: 1px+2px; } }')
      === '.a {\n  color: red;\n}\n.a .b {\n  width: 3px;\n}', 'nesting_descendant');

  // ---------- 5. Arithmetic / units / built-in math ----------
  chk(C('.a{ width: 10px*2; height: 100px/4 + 1px; }')
      === '.a {\n  width: 20px;\n  height: 26px;\n}', 'arith_units');
  chk(C('.a{ x: 10 % 3; y: 2 < 3; }') === '.a {\n  x: 1;\n  y: true;\n}', 'arith_mod_cmp');

  // ---------- 6. @mixin / @include (defaults, keyword, rest, @content) ----------
  chk(C('@mixin box($w, $h:10px){ width:$w; height:$h; }\n.a{ @include box(5px); }')
      === '.a {\n  width: 5px;\n  height: 10px;\n}', 'mixin_default');
  chk(C('@mixin box($w, $h:10px){ width:$w; height:$h; }\n.b{ @include box($h:2px, $w:3px); }')
      === '.b {\n  width: 3px;\n  height: 2px;\n}', 'mixin_keyword');
  chk(C('@mixin m($a, $rest...){ margin: $a $rest; }\n.a{ @include m(1px,2px,3px); }')
      === '.a {\n  margin: 1px 2px, 3px;\n}', 'mixin_rest');
  chk(C('@mixin media($q){ @media (min-width:$q){ @content; } }\n.a{ @include media(100px){ color:red; } }')
      === '@media (min-width: 100px) {\n  .a {\n    color: red;\n  }\n}', 'mixin_content');
  chk(C('@mixin m(){ @content; } .a{ @include m { z: 9; } }') === '.a {\n  z: 9;\n}', 'mixin_content_block');

  // ---------- 7. @function / @return ----------
  chk(C('@function double($n){ @return $n*2; }\n.a{ width: double(4px); }')
      === '.a {\n  width: 8px;\n}', 'function_double');

  // ---------- 8. @if / @else if / @else ----------
  chk(C('@mixin t($v){ @if $v > 10 { x:big; } @else if $v > 5 { x:med; } @else { x:small; } }\n.a{ @include t(7); }')
      === '.a {\n  x: med;\n}', 'if_elseif');
  chk(C('@mixin t($v){ @if $v > 10 { x:big; } @else if $v > 5 { x:med; } @else { x:small; } }\n.a{ @include t(2); }')
      === '.a {\n  x: small;\n}', 'if_else');

  // ---------- 9. @each / @for / @while ----------
  chk(C('@each $i in a, b, c { .#{$i}{ x:1; } }')
      === '.a {\n  x: 1;\n}\n\n.b {\n  x: 1;\n}\n\n.c {\n  x: 1;\n}', 'each');
  chk(C('@for $i from 1 through 3 { .m#{$i}{ w:$i; } }')
      === '.m1 {\n  w: 1;\n}\n\n.m2 {\n  w: 2;\n}\n\n.m3 {\n  w: 3;\n}', 'for_through');
  chk(C('$i:1; @while $i<=3 { .w#{$i}{ a:$i; } $i: $i+1; }')
      === '.w1 {\n  a: 1;\n}\n\n.w2 {\n  a: 2;\n}\n\n.w3 {\n  a: 3;\n}', 'while');
  chk(C('$m:(a:1,b:2); @each $k,$v in $m { .#{$k}{ x:$v; } }')
      === '.a {\n  x: 1;\n}\n\n.b {\n  x: 2;\n}', 'each_map_kv');

  // ---------- 10. Interpolation ----------
  chk(C('$name: foo; .#{$name}-bar{ content: "#{$name}"; }')
      === '.foo-bar {\n  content: "foo";\n}', 'interpolation');

  // ---------- 11. Placeholder %x + @extend (incl. !optional, chains) ----------
  chk(C('%base{ color:red; } .a{ @extend %base; } .b{ @extend %base; }')
      === '.b, .a {\n  color: red;\n}', 'placeholder_extend');
  chk(C('.a{ @extend .nonexistent !optional; color:red; }') === '.a {\n  color: red;\n}', 'extend_optional');
  chk(C('%a{ x:1; } %b{ y:2; } .c{ @extend %a; @extend %b; }')
      === '.c {\n  x: 1;\n}\n\n.c {\n  y: 2;\n}', 'extend_multi');

  // ---------- 12. @media nested / @at-root / nested props / !important ----------
  chk(C('.a{ color:red; @media (min-width:100px){ color:blue; } }')
      === '.a {\n  color: red;\n}\n@media (min-width: 100px) {\n  .a {\n    color: blue;\n  }\n}', 'media_nested');
  chk(C('.parent{ .child{ @at-root .escaped{ color:red; } } }')
      === '.escaped {\n  color: red;\n}', 'at_root');
  chk(C('.a{ font: { family: serif; size: 12px; } }')
      === '.a {\n  font-family: serif;\n  font-size: 12px;\n}', 'nested_props');
  chk(C('.a{ color: red !important; }') === '.a {\n  color: red !important;\n}', 'important');

  // ---------- 13. Comments / charset ----------
  chk(C('/* keep */\n.a{ color: red; // silent\n}')
      === '/* keep */\n.a {\n  color: red;\n}', 'comment_loud');
  chk(C('/* keep */\n.a{ color: red; }', { style: 'compressed' }) === '.a{color:red}', 'comment_compressed');
  chk(C('.a{ content: "café"; }') === '@charset "UTF-8";\n.a {\n  content: "café";\n}', 'charset_nonascii');
  chk(C('.a{ content: "café"; }', { charset: false }) === '.a {\n  content: "café";\n}', 'charset_false');

  // ---------- 14. @use built-in modules: sass:math ----------
  chk(C('@use "sass:math";\n.a{ d: math.div(100px,4); p: math.pow(2,3); c: math.ceil(4.2); }')
      === '.a {\n  d: 25px;\n  p: 8;\n  c: 5;\n}', 'math_div_pow_ceil');
  chk(C('@use "sass:math";\n.a{ mx: math.max(1,5,3); mn: math.min(2,8); ab: math.abs(-3); rn: math.round(4.6); pc: math.percentage(0.25); }')
      === '.a {\n  mx: 5;\n  mn: 2;\n  ab: 3;\n  rn: 5;\n  pc: 25%;\n}', 'math_funcs');
  chk(C('@use "sass:math";\n.a{ x: math.div(10px,2px); }') === '.a {\n  x: 5;\n}', 'math_div_units_cancel');

  // ---------- 15. @use sass:string ----------
  chk(C('@use "sass:string";\n.a{ len: string.length("hello"); up: string.to-upper-case("abc"); idx: string.index("abcd","c"); sub: string.slice("hamburger",4,6); }')
      === '.a {\n  len: 5;\n  up: "ABC";\n  idx: 3;\n  sub: "bur";\n}', 'string_funcs');
  chk(C('@use "sass:string";\n.a{ q: string.quote(hello); uq: string.unquote("world"); ins: string.insert("abc","X",2); }')
      === '.a {\n  q: "hello";\n  uq: world;\n  ins: "aXbc";\n}', 'string_quote_unquote_insert');

  // ---------- 16. @use sass:color ----------
  chk(C('@use "sass:color";\n.a{ c: color.adjust(#6b717f, $red:15); }') === '.a {\n  c: #7a717f;\n}', 'color_adjust');
  chk(C('@use "sass:color";\n.a{ c: color.adjust(#ff0000, $lightness: -20%); }') === '.a {\n  c: #990000;\n}', 'color_adjust_lightness');
  chk(C('@use "sass:color";\n.a{ c: color.complement(red); }') === '.a {\n  c: aqua;\n}', 'color_complement');
  chk(C('@use "sass:color";\n.a{ c: color.mix(white, black); }') === '.a {\n  c: rgb(127.5, 127.5, 127.5);\n}', 'color_mix');
  chk(C('@use "sass:color";\n.a{ r: color.channel(#ff0000, "red"); }') === '.a {\n  r: 255;\n}', 'color_channel');
  chk(C('.a{ c: rgba(255,0,0,0.5); }') === '.a {\n  c: rgba(255, 0, 0, 0.5);\n}', 'rgba');
  chk(C('.a{ c: hsl(0, 100%, 50%); }') === '.a {\n  c: hsl(0, 100%, 50%);\n}', 'hsl');

  // ---------- 17. @use sass:list ----------
  chk(C('@use "sass:list";\n.a{ len: list.length(1px 2px 3px); nth: list.nth(a b c, 2); join: list.join(1px 2px, 3px 4px); }')
      === '.a {\n  len: 3;\n  nth: b;\n  join: 1px 2px 3px 4px;\n}', 'list_funcs');
  chk(C('@use "sass:list";\n.a{ sep: list.separator((1px, 2px)); app: list.append(1px 2px, 3px); idx: list.index(1px 2px 3px, 2px); }')
      === '.a {\n  sep: comma;\n  app: 1px 2px 3px;\n  idx: 2;\n}', 'list_sep_append_index');

  // ---------- 18. @use sass:map ----------
  chk(C('@use "sass:map";\n$m: (a:1, b:2);\n.a{ g: map.get($m,a); keys: map.keys($m); vals: map.values($m); has: map.has-key($m,b); }')
      === '.a {\n  g: 1;\n  keys: a, b;\n  vals: 1, 2;\n  has: true;\n}', 'map_funcs');
  chk(C('@use "sass:map";\n$m: (a:1);\n$m2: map.merge($m, (b:2));\n.a{ b: map.get($m2,b); }')
      === '.a {\n  b: 2;\n}', 'map_merge');
  chk(C('@use "sass:map";\n$m: (a:1);\n$m2: map.set($m, c, 3);\n.a{ c: map.get($m2,c); }')
      === '.a {\n  c: 3;\n}', 'map_set');

  // ---------- 19. @use namespace alias / wildcard ----------
  chk(C('@use "sass:math" as m;\n.a{ x: m.div(10,2); }') === '.a {\n  x: 5;\n}', 'use_alias');
  chk(C('@use "sass:math" as *;\n.a{ x: div(10,2); }') === '.a {\n  x: 5;\n}', 'use_star');

  // ---------- 20. Indented (.sass) syntax option ----------
  chk(C('a\n  color: red', { syntax: 'indented' }) === 'a {\n  color: red;\n}', 'indented_syntax');

  // ---------- 21. File-based compile() + compileAsync() ----------
  fs.mkdirSync(TMP, { recursive: true });
  const mainFile = path.join(TMP, 'main.scss');
  fs.writeFileSync(mainFile, '$pad: 4px;\n.box{ padding: $pad; }\n');
  chk(sass.compile(mainFile, { logger: SILENT }).css === '.box {\n  padding: 4px;\n}', 'compile_file');
  const af = await sass.compileAsync(mainFile, { logger: SILENT });
  chk(af.css === '.box {\n  padding: 4px;\n}', 'compileAsync_file');

  // ---------- 22. loadPaths (@use a written partial) ----------
  fs.writeFileSync(path.join(TMP, '_lib.scss'), '$gap: 8px;\n@mixin g(){ gap: $gap; }\n');
  chk(C('@use "lib";\n.a{ @include lib.g(); }', { loadPaths: [TMP] }) === '.a {\n  gap: 8px;\n}', 'loadPaths_use');

  // ---------- 23. Custom importer (in-memory module) ----------
  const imp = sass.compileString('@use "virtual";\n.a{ color: virtual.$c; }', {
    logger: SILENT,
    importers: [{
      canonicalize(url) { return (url === 'virtual' || url === 'virtual.scss') ? new URL('mem:virtual') : null; },
      load() { return { contents: '$c: tomato;', syntax: 'scss' }; }
    }]
  });
  chk(imp.css === '.a {\n  color: tomato;\n}', 'custom_importer');

  // ---------- 24. Custom JS function (functions option) ----------
  const fnRes = sass.compileString('.a{ w: triple(4px); }', {
    logger: SILENT,
    functions: {
      'triple($n)': (args) => { const n = args[0].assertNumber('n'); return new sass.SassNumber(n.value * 3, 'px'); }
    }
  });
  chk(fnRes.css === '.a {\n  w: 12px;\n}', 'custom_function');

  // ---------- 25. Async string compile + Compiler reuse ----------
  const asyncRes = await sass.compileStringAsync('.a{ b: 1+1; }', { logger: SILENT });
  chk(asyncRes.css === '.a {\n  b: 2;\n}', 'compileStringAsync');
  const compiler = sass.initCompiler();
  chk(compiler.compileString('.a{ x: 2*2; }', { logger: SILENT }).css === '.a {\n  x: 4;\n}', 'compiler_reuse_1');
  chk(compiler.compileString('.b{ y: 3*3; }', { logger: SILENT }).css === '.b {\n  y: 9;\n}', 'compiler_reuse_2');
  compiler.dispose();
  const acompiler = await sass.initAsyncCompiler();
  const ac = await acompiler.compileStringAsync('.c{ z: 5+5; }', { logger: SILENT });
  chk(ac.css === '.c {\n  z: 10;\n}', 'async_compiler');
  await acompiler.dispose();

  // ---------- 26. sourceMap + loadedUrls ----------
  const sm = sass.compileString('.a{ color: red; }', { sourceMap: true, logger: SILENT });
  chk(!!sm.sourceMap && sm.sourceMap.version === 3, 'sourceMap_v3');
  const lu = sass.compileString('.a{ b: 1; }', { logger: SILENT, url: new URL('mem:entry') });
  chk(lu.loadedUrls.map(u => u.href).join(',') === 'mem:entry', 'loadedUrls');
  // no sourceMap requested -> undefined
  chk(sass.compileString('.a{ b: 1; }', { logger: SILENT }).sourceMap === undefined, 'no_sourceMap');

  // ---------- 27. Value type constructors ----------
  chk(new sass.SassNumber(5).value === 5, 'SassNumber_value');
  chk(new sass.SassNumber(3, 'px').toString() === '3px', 'SassNumber_unit_toString');
  chk(new sass.SassString('hi').text === 'hi', 'SassString_text');
  chk(sass.sassTrue.value === true && sass.sassFalse.value === false, 'SassBoolean_values');
  chk(sass.sassNull.realNull === null, 'sassNull');
  chk(new sass.SassColor({ red: 255, green: 0, blue: 0, space: 'rgb' }).channel('red') === 255, 'SassColor_channel');
  const slist = new sass.SassList([new sass.SassNumber(1), new sass.SassNumber(2)], { separator: ',' });
  chk(slist.asList.size === 2 && slist.separator === ',', 'SassList_ctor');
  const smap = new sass.SassMap(new Map([[new sass.SassString('k'), new sass.SassNumber(7)]]));
  chk(smap.contents.size === 1, 'SassMap_ctor');

  // ---------- 28. Error path: invalid scss throws sass.Exception with span ----------
  let threw = false, exObj = null;
  try { sass.compileString('.a{ color: ; }', { logger: SILENT }); }
  catch (e) { threw = true; exObj = e; }
  chk(threw === true, 'error_throws');
  chk(exObj instanceof sass.Exception, 'error_is_Exception');
  chk(exObj && exObj.sassMessage === 'Expected expression.', 'error_message');
  chk(exObj && exObj.span && exObj.span.start && exObj.span.start.offset === 11, 'error_span_offset');
  chk(exObj && exObj.span.start.line === 0 && exObj.span.start.column === 11, 'error_span_line_col');
  let threw2 = false, msg2 = '';
  try { sass.compileString('.a{ @include nope; }', { logger: SILENT }); }
  catch (e) { threw2 = true; msg2 = e.sassMessage; }
  chk(threw2 && msg2 === 'Undefined mixin.', 'error_undefined_mixin');

  // ---------- 29. Logger @warn / @debug capture ----------
  const warns = [];
  sass.compileString('@warn "hi there";\n.a{ b: 1; }', { logger: { warn(m) { warns.push(m); } } });
  chk(warns.length === 1 && warns[0] === 'hi there', 'logger_warn');
  const debugs = [];
  sass.compileString('@debug "dbg";\n.a{ b: 1; }', { logger: { debug(m) { debugs.push(m); } } });
  chk(debugs.length === 1 && debugs[0] === 'dbg', 'logger_debug');

  // ---------- 30. Legacy API render() / renderSync() ----------
  const rsync = sass.renderSync({ data: '.a{ b: 2+2; }' }).css.toString();
  chk(rsync === '.a {\n  b: 4;\n}', 'legacy_renderSync');
  const rcb = await new Promise((resolve, reject) => {
    sass.render({ data: '.a{ b: 1+1; }' }, (err, res) => err ? reject(err) : resolve(res.css.toString()));
  });
  chk(rcb === '.a {\n  b: 2;\n}', 'legacy_render_cb');

  // cleanup scratch dir
  fs.rmSync(TMP, { recursive: true, force: true });
}

main().then(() => {
  console.log('SCSS_RESULT ok=' + ok + ' fail=' + fail);
  if (fail === 0) console.log('SCSS_DONE');
  process.exit(fail === 0 ? 0 : 1);
}).catch((e) => {
  try { fs.rmSync(TMP, { recursive: true, force: true }); } catch (_) {}
  console.log('FATAL ' + (e && e.stack ? e.stack : e));
  console.log('SCSS_RESULT ok=' + ok + ' fail=' + (fail + 1));
  process.exit(1);
});
