'use strict';
/*
 * INDUSTRIAL carpet for terser 5.37.0 on Node.js v22.22.2.
 * Every assertion is an exact-value check against goldens captured by
 * running terser on this exact version. Deterministic only:
 * no Date.now / Math.random / wall-clock / network.
 * Portable: terser is required relative to this file (__dirname), no
 * hardcoded host paths; no scratch file IO is performed.
 */

// terser resolves from this file's directory regardless of cwd.
const terser = require('terser');
const { minify, minify_sync } = terser;

let ok = 0, fail = 0;
function chk(cond, name) {
  if (cond) ok++;
  else { fail++; console.log('FAIL ' + name); }
}
function eq(actual, expected, name) {
  if (actual === expected) ok++;
  else {
    fail++;
    console.log('FAIL ' + name + '\n   expected ' + JSON.stringify(expected) +
                '\n   actual   ' + JSON.stringify(actual));
  }
}

(async () => {
  // ---- module surface ----------------------------------------------------
  chk(typeof minify === 'function', 'minify-is-function');
  chk(typeof minify_sync === 'function', 'minify_sync-is-function');
  chk(typeof terser._default_options === 'function', '_default_options-is-function');
  chk(minify('var x=1;') instanceof Promise, 'minify-returns-promise');
  chk(require('terser/package.json').version === '5.37.0', 'version-5.37.0');

  let r;

  // ---- basic default minify (compress + mangle on by default) ------------
  r = await minify('function foo(bar){return bar+1;}');
  eq(r.code, 'function foo(n){return n+1}', 'default-minify');
  chk(typeof r.code === 'string', 'default-code-string');
  chk(r.error === undefined, 'default-no-error');
  chk(r.map === undefined, 'default-no-map');

  r = await minify('let x = 1 + 2; console.log(x);');
  eq(r.code, 'let x=3;console.log(x);', 'default-evaluate-let');

  // ---- mangle variants ---------------------------------------------------
  r = await minify('function add(first, second){return first + second;}', { mangle: true });
  eq(r.code, 'function add(n,d){return n+d}', 'mangle-true');

  r = await minify('function add(first, second){return first + second;}', { mangle: { toplevel: true } });
  eq(r.code, 'function n(n,r){return n+r}', 'mangle-toplevel');

  r = await minify('function add(aaa, bbb){return aaa + bbb;}', { mangle: { reserved: ['aaa'] } });
  eq(r.code, 'function add(aaa,n){return aaa+n}', 'mangle-reserved');

  r = await minify('var o = {}; o._private = 1; o._other = 2; console.log(o._private);',
                   { mangle: { properties: { regex: /^_/ } }, compress: false });
  eq(r.code, 'var o={};o.o=1;o.l=2;console.log(o.o);', 'mangle-properties-regex');

  r = await minify('var o = {a:1, b:2}; o.a; o["b"];',
                   { mangle: { properties: { keep_quoted: true } }, compress: false });
  eq(r.code, 'var o={a:1,b:2};o.a;o["b"];', 'mangle-properties-keep_quoted');

  // ---- compress: individual transforms -----------------------------------
  r = await minify('function f(){ if(false){ return 1; } return 2; }', { compress: { dead_code: true }, mangle: false });
  eq(r.code, 'function f(){return 2}', 'compress-dead_code');

  r = await minify('console.log("a"); var x=1;', { compress: { drop_console: true }, mangle: false });
  eq(r.code, 'var x=1;', 'compress-drop_console');

  r = await minify('console.log(1);console.error(2);', { compress: { drop_console: ['log'] }, mangle: false });
  eq(r.code, 'console.error(2);', 'compress-drop_console-array');

  r = await minify('function f(){ debugger; return 1; }', { compress: { drop_debugger: true }, mangle: false });
  eq(r.code, 'function f(){return 1}', 'compress-drop_debugger');

  r = await minify('function f(){ if(a) return 1; return 2; }', { compress: { conditionals: true, if_return: true }, mangle: false });
  eq(r.code, 'function f(){return a?1:2}', 'compress-conditionals-if_return');

  r = await minify('var x = 2 + 3 * 4;', { compress: { evaluate: true }, mangle: false });
  eq(r.code, 'var x=14;', 'compress-evaluate');

  r = await minify('var s = "foo" + "bar";', { compress: { evaluate: true }, mangle: false });
  eq(r.code, 'var s="foobar";', 'compress-evaluate-strings');

  r = await minify('function f(){ var x = 5; return x + 1; }', { compress: { collapse_vars: true, unused: false }, mangle: false });
  eq(r.code, 'function f(){var x=5;return 6}', 'compress-collapse_vars');

  r = await minify('function f(){ var x = 5; var y = 10; return y; }', { compress: { unused: true }, mangle: false });
  eq(r.code, 'function f(){return 10}', 'compress-unused');

  r = await minify('var x = 1; var y = x + 1; var z = y + 1; console.log(z);', { compress: { passes: 2 }, mangle: false });
  eq(r.code, 'var x=1,y=x+1,z=y+1;console.log(z);', 'compress-passes2');

  r = await minify('function f(){ pure(1); return 2; }', { compress: { pure_funcs: ['pure'] }, mangle: false });
  eq(r.code, 'function f(){return 2}', 'compress-pure_funcs');

  r = await minify('a();b();c();', { compress: { sequences: false }, mangle: false });
  eq(r.code, 'a();b();c();', 'compress-sequences-off');

  r = await minify('a();b();c();', { compress: { sequences: true }, mangle: false });
  eq(r.code, 'a(),b(),c();', 'compress-sequences-on');

  r = await minify('var a=1; var b=2;', { compress: { join_vars: true }, mangle: false });
  eq(r.code, 'var a=1,b=2;', 'compress-join_vars');

  r = await minify('var unusedVar = 5; function used(){return 1;} used();', { compress: { toplevel: true, unused: true }, mangle: false });
  eq(r.code, '', 'compress-toplevel-unused');

  r = await minify('if(typeof x === "undefined"){}', { compress: { typeofs: true }, mangle: false });
  eq(r.code, '', 'compress-typeofs');

  r = await minify('if(DEBUG){console.log("x");} keep();', { compress: { global_defs: { DEBUG: false } }, mangle: false });
  eq(r.code, 'keep();', 'compress-global_defs');

  r = await minify('(function(){foo();})();', { compress: true, mangle: false });
  eq(r.code, 'foo();', 'compress-iife-inline');

  r = await minify('for(var i=0;i<10;i++){a();b();}', { compress: true, mangle: false });
  eq(r.code, 'for(var i=0;i<10;i++)a(),b();', 'compress-for-sequences');

  // booleans: !!a in value position stays
  r = await minify('var x = !!a;', { compress: { booleans: true }, mangle: false });
  eq(r.code, 'var x=!!a;', 'compress-booleans-value');

  // ---- format / output options ------------------------------------------
  r = await minify('function f(){return 1;}', { format: { beautify: true }, mangle: false, compress: false });
  eq(r.code, 'function f() {\n    return 1;\n}', 'format-beautify');

  r = await minify('function f(){return 1;}', { format: { beautify: true, indent_level: 2 }, mangle: false, compress: false });
  eq(r.code, 'function f() {\n  return 1;\n}', 'format-indent_level');

  r = await minify('var a=1;var b=2;', { output: { beautify: true }, compress: false, mangle: false });
  eq(r.code, 'var a = 1;\n\nvar b = 2;', 'format-output-alias');

  r = await minify('/* keep */ function f(){return 1;} // line', { format: { comments: 'all' }, mangle: false, compress: false });
  eq(r.code, '/* keep */function f(){return 1}// line', 'format-comments-all');

  r = await minify('/*! bang */ function f(){return 1;}', { format: { comments: true }, mangle: false, compress: false });
  eq(r.code, '/*! bang */function f(){return 1}', 'format-comments-true');

  r = await minify('/*@preserve me*/ /*nope*/ var x=1;', { format: { comments: /@preserve/ }, mangle: false, compress: false });
  eq(r.code, '/*@preserve me*/var x=1;', 'format-comments-regex');

  r = await minify('/*@license keep*/ /*drop*/ var x=1;',
                   { format: { comments: function (node, comment) { return /@license/.test(comment.value); } }, mangle: false, compress: false });
  eq(r.code, '/*@license keep*/var x=1;', 'format-comments-function');

  r = await minify('var a=1;var b=2;', { format: { semicolons: false }, mangle: false, compress: false });
  eq(r.code, 'var a=1\nvar b=2\n', 'format-semicolons-false');

  r = await minify('var s = "é中";', { format: { ascii_only: true }, mangle: false, compress: false });
  eq(r.code, 'var s="\\xe9\\u4e2d";', 'format-ascii_only');

  r = await minify('var s = "hello";', { format: { quote_style: 1 }, mangle: false, compress: false });
  eq(r.code, "var s='hello';", 'format-quote_style-1-single');

  r = await minify("var s = 'hello';", { format: { quote_style: 0 }, mangle: false, compress: false });
  eq(r.code, 'var s="hello";', 'format-quote_style-0-prefer-double');

  r = await minify("var s = 'hello';", { format: { quote_style: 3 }, mangle: false, compress: false });
  eq(r.code, "var s='hello';", 'format-quote_style-3-original');

  r = await minify('var o = {a:1, b:2};', { format: { quote_keys: true }, compress: false, mangle: false });
  eq(r.code, 'var o={"a":1,"b":2};', 'format-quote_keys');

  r = await minify('var s = "</script>";', { format: { inline_script: true }, compress: false, mangle: false });
  eq(r.code, 'var s="<\\/script>";', 'format-inline_script');

  r = await minify('var aaa=1;var bbb=2;var ccc=3;', { format: { max_line_len: 10 }, compress: false, mangle: false });
  eq(r.code, 'var aaa=1\n;var bbb=2\n;var ccc=3;', 'format-max_line_len');

  r = await minify('var x=1;', { format: { preamble: '/* banner */' }, compress: false, mangle: false });
  eq(r.code, '/* banner */\nvar x=1;', 'format-preamble');

  // ---- ecma targets ------------------------------------------------------
  r = await minify('var o = { foo: foo, bar: bar };', { ecma: 2015, mangle: false, compress: true });
  eq(r.code, 'var o={foo,bar};', 'ecma2015-shorthand');

  r = await minify('var x = a ?? b;', { ecma: 2020, mangle: false, compress: false });
  eq(r.code, 'var x=a??b;', 'ecma2020-nullish');

  r = await minify('[1,2].map(function(x){return x*2;});', { compress: true, mangle: false, ecma: 5 });
  eq(r.code, '[1,2].map((function(x){return 2*x}));', 'ecma5-no-arrow');

  r = await minify('const f = (...args) => args.reduce((a,b)=>a+b, 0); console.log(f(1,2,3));',
                   { compress: true, mangle: true, ecma: 2015 });
  eq(r.code, 'const f=(...o)=>o.reduce(((o,c)=>o+c),0);console.log(f(1,2,3));', 'ecma2015-arrow-spread');

  // ---- module mode -------------------------------------------------------
  r = await minify('function foo(){return 1} export { foo };', { module: true, mangle: true });
  eq(r.code, 'function n(){return 1}export{n as foo};', 'module-mode-mangle-export');

  // ---- parse options -----------------------------------------------------
  r = await minify('return 42;', { parse: { bare_returns: true }, compress: false, mangle: false });
  eq(r.code, 'return 42;', 'parse-bare_returns');

  // ---- keep_classnames / keep_fnames ------------------------------------
  r = await minify('class MyClass{ method(){return 1;} } new MyClass().method();',
                   { mangle: { toplevel: true }, keep_classnames: true, compress: false });
  eq(r.code, 'class MyClass{method(){return 1}}(new MyClass).method();', 'keep_classnames-true');

  r = await minify('class MyClass{ method(){return 1;} } new MyClass().method();',
                   { mangle: { toplevel: true }, keep_classnames: false, compress: false });
  eq(r.code, 'class e{method(){return 1}}(new e).method();', 'keep_classnames-false');

  r = await minify('function longFnName(){return 1;} longFnName();',
                   { mangle: { toplevel: true }, keep_fnames: true, compress: false });
  eq(r.code, 'function longFnName(){return 1}longFnName();', 'keep_fnames-true');

  r = await minify('function longFnName(){return 1;} longFnName();',
                   { mangle: { toplevel: true }, keep_fnames: false, compress: false });
  eq(r.code, 'function n(){return 1}n();', 'keep_fnames-false');

  // ---- whitespace-only (no compress, no mangle) --------------------------
  r = await minify('function  f ( a ,  b ) {  return  a  +  b ; }', { compress: false, mangle: false });
  eq(r.code, 'function f(a,b){return a+b}', 'whitespace-only');

  // ---- number formatting -------------------------------------------------
  r = await minify('var x = 1000000;', { compress: false, mangle: false });
  eq(r.code, 'var x=1e6;', 'number-1e6');
  r = await minify('var x = 0.5;', { compress: false, mangle: false });
  eq(r.code, 'var x=.5;', 'number-leading-dot');
  r = await minify('var x = 255;', { compress: false, mangle: false });
  eq(r.code, 'var x=255;', 'number-255');

  // ---- multiple files (object form) -------------------------------------
  r = await minify({ 'a.js': 'var a = 1;', 'b.js': 'var b = 2; console.log(a+b);' }, { compress: false, mangle: false });
  eq(r.code, 'var a=1;var b=2;console.log(a+b);', 'multifile-object-form');

  // ---- realistic end-to-end default --------------------------------------
  r = await minify([
    'function calculate(input) {',
    '  var unused = 999;',
    '  var base = 10;',
    '  var result = base * input;',
    '  if (false) { result = 0; }',
    '  return result;',
    '}',
    'console.log(calculate(5));'
  ].join('\n'));
  eq(r.code, 'function calculate(c){var l=10*c;return l}console.log(calculate(5));', 'realistic-default');

  // ---- sourceMap ---------------------------------------------------------
  r = await minify('function foo(bar){return bar+1;}', { sourceMap: true });
  chk(typeof r.map === 'string', 'sourceMap-map-is-string');
  let m = JSON.parse(r.map);
  eq(m.version, 3, 'sourceMap-version-3');
  chk(typeof m.mappings === 'string' && m.mappings.length > 0, 'sourceMap-has-mappings');

  r = await minify({ 'in.js': 'function foo(bar){return bar+1;}' }, { sourceMap: { url: 'out.js.map', filename: 'out.js' } });
  eq(r.code.split('\n').pop(), '//# sourceMappingURL=out.js.map', 'sourceMap-url-comment');
  m = JSON.parse(r.map);
  eq(JSON.stringify(m.sources), JSON.stringify(['in.js']), 'sourceMap-sources');
  eq(m.file, 'out.js', 'sourceMap-file');

  r = await minify({ 'in.js': 'var x=1;' }, { sourceMap: { includeSources: true } });
  m = JSON.parse(r.map);
  eq(JSON.stringify(m.sourcesContent), JSON.stringify(['var x=1;']), 'sourceMap-includeSources');

  // ---- nameCache: two-call name stability --------------------------------
  const cache = {};
  let r1 = await minify('function libFunc(longArgName){return longArgName+1;} libFunc(globalThing);',
                        { mangle: { toplevel: true }, nameCache: cache, compress: false });
  eq(r1.code, 'function n(n){return n+1}n(globalThing);', 'nameCache-call1');
  chk(!!cache.vars, 'nameCache-populated');
  let r2 = await minify('libFunc(anotherGlobal);', { mangle: { toplevel: true }, nameCache: cache, compress: false });
  eq(r2.code, 'n(anotherGlobal);', 'nameCache-call2-stable');

  // ---- minify_sync -------------------------------------------------------
  let rs = minify_sync('function f(x){return x*2;}');
  eq(rs.code, 'function f(n){return 2*n}', 'minify_sync-valid');
  chk(rs.error === undefined, 'minify_sync-no-error');

  // ---- empty input -------------------------------------------------------
  r = await minify('');
  eq(r.code, '', 'empty-input');

  // ---- error path: invalid JS rejects (async) and throws (sync) ----------
  let threw = false, errname = '';
  try { await minify('function (){'); }
  catch (e) { threw = true; errname = e.name; }
  chk(threw, 'invalid-js-async-rejects');
  eq(errname, 'SyntaxError', 'invalid-js-error-name');

  let threwSync = false;
  try { minify_sync('var a = ;'); }
  catch (e) { threwSync = true; }
  chk(threwSync, 'invalid-js-sync-throws');

  // ---- final result line -------------------------------------------------
  console.log('TERSER_RESULT ok=' + ok + ' fail=' + fail);
  if (fail === 0) console.log('TERSER_DONE');
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.log('FATAL ' + (e && e.stack ? e.stack : e));
  console.log('TERSER_RESULT ok=' + ok + ' fail=' + (fail + 1));
  process.exit(1);
});
