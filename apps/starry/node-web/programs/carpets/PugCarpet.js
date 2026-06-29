'use strict';
/*
 * PugCarpet.js — INDUSTRIAL-grade carpet for pug 3.0.3 on Node.js 22 LTS.
 * Every assertion is an exact-value check (=== against a golden observed from
 * the real pug 3.0.3 install on this exact node). Deterministic only.
 *
 * Run:
 *   cd <nweb> && node PugCarpet.js
 */
const pug = require('pug');
const fs = require('fs');
const path = require('path');

let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }

// eq: render `src` with `locals/opts` and assert it equals exact `expected`.
function eq(name, src, expected, optsLocals) {
  let got;
  try { got = pug.render(src, optsLocals || {}); }
  catch (e) { fail++; console.log('FAIL ' + name + ' (threw: ' + e.message + ')'); return; }
  if (got === expected) { ok++; }
  else { fail++; console.log('FAIL ' + name + '\n  expected: ' + JSON.stringify(expected) + '\n  got:      ' + JSON.stringify(got)); }
}

// ---------------------------------------------------------------------------
// Helper template files (self-contained: written fresh into pugtmpl_carpet/)
// ---------------------------------------------------------------------------
const TDIR = path.join(process.cwd(), 'pugtmpl_carpet');
fs.mkdirSync(TDIR, { recursive: true });
function w(rel, content) { fs.writeFileSync(path.join(TDIR, rel), content); }
w('layout.pug',
  'html\n  head\n    block title\n      title Default\n  body\n    block content\n      p default content\n    block footer\n      footer base\n');
w('child.pug',
  'extends layout\nblock title\n  title Home\nblock content\n  h1 Welcome\n');
w('childappend.pug',
  'extends layout\nblock append content\n  p appended\nblock prepend content\n  p prepended\n');
w('partial.pug', 'p partial-included\n');
w('withinclude.pug', 'p before include\ninclude partial\np after include\n');
w('raw.txt', 'raw text line1\nraw text line2\n');
w('withrawtxt.pug', 'div.box\n  include raw.txt\n');
const FP = (rel) => path.join(TDIR, rel);

// ===========================================================================
// 1. API SURFACE
// ===========================================================================
chk(pug.name === 'Pug', 'api.name');
chk(typeof pug.render === 'function', 'api.render-fn');
chk(typeof pug.renderFile === 'function', 'api.renderFile-fn');
chk(typeof pug.compile === 'function', 'api.compile-fn');
chk(typeof pug.compileFile === 'function', 'api.compileFile-fn');
chk(typeof pug.compileClient === 'function', 'api.compileClient-fn');
chk(typeof pug.compileClientWithDependenciesTracked === 'function', 'api.ccwdt-fn');
chk(typeof pug.compileFileClient === 'function', 'api.compileFileClient-fn');
chk(typeof pug.__express === 'function', 'api.__express-fn');
chk(typeof pug.runtime === 'object' && pug.runtime !== null, 'api.runtime-obj');
chk(pug.cache && typeof pug.cache === 'object', 'api.cache-obj');
chk(pug.filters && typeof pug.filters === 'object', 'api.filters-obj');

// ===========================================================================
// 2. TAGS & SHORTHAND
// ===========================================================================
eq('tag.basic', 'p Hello world', '<p>Hello world</p>');
eq('tag.classid', 'a.btn.big#go(href="/x") Go', '<a class="btn big" id="go" href="/x">Go</a>');
eq('tag.div-implied', '.card#main content', '<div class="card" id="main">content</div>');
eq('tag.img-selfclose', 'img(src="a.png" alt="x")', '<img src="a.png" alt="x"/>');
eq('tag.nesting', 'ul\n  li one\n  li two', '<ul><li>one</li><li>two</li></ul>');
eq('tag.interp', 'p before #[a(href="x") y] after', '<p>before <a href="x">y</a> after</p>');
eq('tag.interp-class', 'p #[strong.hl word] end', '<p><strong class="hl">word</strong> end</p>');
eq('tag.block-expansion', 'a: img(src="x")', '<a><img src="x"/></a>');
eq('tag.dot-text', 'p.\n  This is\n  plain text', '<p>This is\nplain text</p>');
eq('tag.pipe-text', 'p\n  | line one\n  | line two', '<p>line one\nline two</p>');
eq('tag.script-raw', 'script.\n  var x = 1 < 2;', '<script>var x = 1 < 2;</script>');
eq('tag.text-interp', 'p Hello #{who}!', '<p>Hello world!</p>', { who: 'world' });
eq('tag.literal-eq', 'p.\n  1 + 1 = 2', '<p>1 + 1 = 2</p>');

// ===========================================================================
// 3. ATTRIBUTES
// ===========================================================================
eq('attr.bool-true', 'input(type="checkbox" checked)', '<input type="checkbox" checked="checked"/>');
eq('attr.bool-false', 'input(type="checkbox" checked=false)', '<input type="checkbox"/>');
eq('attr.disabled-true', 'input(disabled=true)', '<input disabled="disabled"/>');
eq('attr.interp', 'a(href="/u/"+name) link', '<a href="/u/bob">link</a>', { name: 'bob' });
eq('attr.escape-quote', "a(title='a\"b') t", '<a title="a&quot;b">t</a>');
eq('attr.class-array', 'a(class=["a","b","c"]) x', '<a class="a b c">x</a>');
eq('attr.class-object', 'a(class={active:true,disabled:false}) x', '<a class="active">x</a>');
eq('attr.style-object', 'a(style={color:"red","font-size":"12px"}) x', '<a style="color:red;font-size:12px;">x</a>');
eq('attr.style-string', 'a(style="color:red") x', '<a style="color:red">x</a>');
eq('attr.data', 'div(data-id=5 data-name="x") y', '<div data-id="5" data-name="x">y</div>');
eq('attr.and-attributes', 'div#a(class="b")&attributes({id:"c",foo:"bar"}) z', '<div class="b" id="c" foo="bar">z</div>');
eq('attr.class-merge', 'a.base&attributes({class:["extra"]}) x', '<a class="base extra">x</a>');
eq('attr.amp-escape', 'a(href="?a=1&b=2") x', '<a href="?a=1&amp;b=2">x</a>');
eq('attr.escaped-default', 'a(data-x="<b>") y', '<a data-x="&lt;b&gt;">y</a>');
eq('attr.unescaped', 'a(data-x!="<b>") y', '<a data-x="<b>">y</a>');
eq('attr.template-interp', 'img(src=`/img/${id}.png`)', '<img src="/img/7.png"/>', { id: 7 });
eq('attr.expr', 'a(tabindex=1+1) x', '<a tabindex="2">x</a>');
eq('attr.eq-and-attr', '- var cls="hl"\np(class=cls)= 1+1', '<p class="hl">2</p>');

// ===========================================================================
// 4. BUFFERED / UNBUFFERED CODE + INTERPOLATION + XSS
// ===========================================================================
eq('code.eq-escaped', 'p= "<b>"+x', '<p>&lt;b&gt;&amp;</p>', { x: '&' });
eq('code.neq-unescaped', 'p!= "<b>"+x', '<p><b>&</p>', { x: '&' });
eq('code.dash-stmt', '- var y = 1 + 2\np= y', '<p>3</p>');
eq('code.interp-esc', 'p #{val}', '<p>&lt;b&gt;</p>', { val: '<b>' });
eq('code.interp-raw', 'p !{val}', '<p><b></p>', { val: '<b>' });
eq('code.xss-escape', 'p #{name}', '<p>&lt;script&gt;</p>', { name: '<script>' });
eq('code.text-quote', 'p= q', '<p>&quot;hi&quot;</p>', { q: '"hi"' });
eq('code.for-loop', 'ul\n  - for(var i=0;i<2;i++)\n    li= i', '<ul><li>0</li><li>1</li></ul>');

// ===========================================================================
// 5. CONDITIONALS
// ===========================================================================
eq('cond.if-elseif-else', 'if n>0\n  p pos\nelse if n<0\n  p neg\nelse\n  p zero', '<p>neg</p>', { n: -5 });
eq('cond.nested-elseif', 'if a\n  p A\nelse if b\n  p B\nelse\n  p C', '<p>B</p>', { a: false, b: true });
eq('cond.unless', 'unless ok\n  p hidden', '<p>hidden</p>', { ok: false });
eq('cond.case', 'case n\n  when 1\n    p one\n  when 2\n    p two\n  default\n    p other', '<p>two</p>', { n: 2 });
eq('cond.case-fallthrough', 'case n\n  when 1\n  when 2\n    p onetwo\n  default\n    p other', '<p>onetwo</p>', { n: 1 });
eq('cond.case-block', 'case n\n  when 1\n    p one\n    p uno\n  default\n    p other', '<p>one</p><p>uno</p>', { n: 1 });
eq('cond.case-default', 'case n\n  when 1\n    p one\n  default\n    p other', '<p>other</p>', { n: 9 });

// ===========================================================================
// 6. ITERATION
// ===========================================================================
eq('iter.each-arr', 'each v in [1,2,3]\n  li= v', '<li>1</li><li>2</li><li>3</li>');
eq('iter.each-idx', 'each v,i in ["a","b"]\n  li #{i}:#{v}', '<li>0:a</li><li>1:b</li>');
eq('iter.each-obj', 'each v,k in {a:1,b:2}\n  li #{k}=#{v}', '<li>a=1</li><li>b=2</li>');
eq('iter.each-keyval', 'each val, key in {x:1,y:2,z:3}\n  p #{key}-#{val}', '<p>x-1</p><p>y-2</p><p>z-3</p>');
eq('iter.each-else', 'each v in []\n  li= v\nelse\n  p empty', '<p>empty</p>');
eq('iter.each-string', 'each c in "ab"\n  li= c', '<li>a</li><li>b</li>');
eq('iter.while', '- var n=0\nwhile n<3\n  li= n++', '<li>0</li><li>1</li><li>2</li>');

// ===========================================================================
// 7. MIXINS
// ===========================================================================
eq('mixin.basic', 'mixin greet(name)\n  p Hi #{name}\n+greet("Al")', '<p>Hi Al</p>');
eq('mixin.default-arg', 'mixin g(n="X")\n  p= n\n+g()\n+g("Y")', '<p>X</p><p>Y</p>');
eq('mixin.rest-args', 'mixin list(id, ...items)\n  ul(id=id)\n    each i in items\n      li= i\n+list("l", 1, 2, 3)', '<ul id="l"><li>1</li><li>2</li><li>3</li></ul>');
eq('mixin.block', 'mixin art()\n  div.art\n    block\n+art()\n  p inner', '<div class="art"><p>inner</p></div>');
eq('mixin.and-attributes', 'mixin m()\n  div&attributes(attributes)\n+m()(class="x" id="y")', '<div class="x" id="y"></div>');
eq('mixin.block-and-attrs', 'mixin item\n  li&attributes(attributes)\n    block\n+item.active hi', '<li class="active">hi</li>');

// ===========================================================================
// 8. COMMENTS
// ===========================================================================
eq('comment.buffered', '// visible comment\np x', '<!-- visible comment--><p>x</p>');
eq('comment.unbuffered', '//- hidden comment\np x', '<p>x</p>');
eq('comment.block', '//\n  multi\n  line\np x', '<!--multi\nline--><p>x</p>');
eq('comment.conditional', '<!--[if IE]>\nlink(rel="stylesheet")\n<![endif]-->', '<!--[if IE]><link rel="stylesheet"/><![endif]-->');

// ===========================================================================
// 9. DOCTYPE
// ===========================================================================
eq('doctype.html', 'doctype html\nhtml\n  body p', '<!DOCTYPE html><html><body>p</body></html>');
eq('doctype.html-bool-terse', 'doctype html\ninput(type="checkbox" checked)', '<!DOCTYPE html><input type="checkbox" checked>');
eq('doctype.html-option-selected', 'doctype html\noption(selected) Hi', '<!DOCTYPE html><option selected>Hi</option>');
eq('doctype.html-img-noslash', 'doctype html\nimg(src="a.png")', '<!DOCTYPE html><img src="a.png">');
eq('doctype.xml', 'doctype xml\nimg(src="a")', '<?xml version="1.0" encoding="utf-8" ?><img src="a"></img>');
eq('doctype.no-doctype-img-slash', 'img(src="a.png")', '<img src="a.png"/>');
eq('doctype.custom', 'doctype html PUBLIC "-//x"\np hi', '<!DOCTYPE html PUBLIC "-//x"><p>hi</p>');

// ===========================================================================
// 10. OPTIONS: pretty, self, globals, cache, doctype option
// ===========================================================================
eq('opt.pretty-true', 'ul\n  li a\n  li b', '\n<ul>\n  <li>a</li>\n  <li>b</li>\n</ul>', { pretty: true });
eq('opt.pretty-doc', 'doctype html\nbody\n  p hi', '<!DOCTYPE html>\n<body>\n  <p>hi</p>\n</body>', { pretty: true });
eq('opt.pretty-default-false', 'div\n  p x', '<div><p>x</p></div>');
eq('opt.self', 'p= self.name', '<p>z</p>', { name: 'z', self: true });
eq('opt.doctype-option', 'input(checked)', '<input checked>', { doctype: 'html' });
// globals: names referenced from the global object instead of locals
global.SITE_NAME = 'ACME';
eq('opt.globals', 'p= SITE_NAME', '<p>ACME</p>', { globals: ['SITE_NAME'] });

// cache option: identical key returns the SAME cached function entry
const c1 = pug.render('p cached', { cache: true, filename: 'carpet-ckey.pug' });
const c2 = pug.render('p cached', { cache: true, filename: 'carpet-ckey.pug' });
chk(c1 === '<p>cached</p>', 'opt.cache-output');
chk(c2 === '<p>cached</p>', 'opt.cache-output2');
chk('carpet-ckey.pug' in pug.cache, 'opt.cache-key-stored');

// ===========================================================================
// 11. compile() -> fn(locals)
// ===========================================================================
const cfn = pug.compile('p Hi #{name}');
chk(typeof cfn === 'function', 'compile.returns-fn');
chk(cfn({ name: 'Eve' }) === '<p>Hi Eve</p>', 'compile.call1');
chk(cfn({ name: 'Bo' }) === '<p>Hi Bo</p>', 'compile.call2');
chk(Array.isArray(cfn.dependencies), 'compile.fn-dependencies-array');
const cfn2 = pug.compile('p= 1+1');
chk(cfn2() === '<p>2</p>', 'compile.expr-eval');

// ===========================================================================
// 12. compileClient / compileClientWithDependenciesTracked / compileFileClient
// ===========================================================================
const ccSrc = pug.compileClient('p Hello #{name}', { name: 'fnTpl' });
chk(typeof ccSrc === 'string', 'compileClient.is-string');
chk(ccSrc.indexOf('function fnTpl') >= 0, 'compileClient.has-named-fn');
chk(ccSrc.indexOf('pug_html') >= 0, 'compileClient.has-pug_html');
chk(ccSrc.indexOf('function pug_escape') >= 0, 'compileClient.inlines-escape');
const ccDefault = pug.compileClient('p x');
chk(ccDefault.indexOf('function template') >= 0, 'compileClient.default-name');
const ccNoDebug = pug.compileClient('p x', { name: 't3', compileDebug: false });
chk(ccNoDebug.indexOf('pug_rethrow') < 0, 'compileClient.compileDebug-false-no-rethrow');
const ccDebug = pug.compileClient('p x', { name: 't4', compileDebug: true });
chk(ccDebug.indexOf('pug_rethrow') >= 0, 'compileClient.compileDebug-true-has-rethrow');

const ccwdt = pug.compileClientWithDependenciesTracked('p x', { name: 't2' });
chk(ccwdt && typeof ccwdt === 'object', 'ccwdt.is-object');
chk('body' in ccwdt && 'dependencies' in ccwdt, 'ccwdt.has-keys');
chk(typeof ccwdt.body === 'string' && ccwdt.body.indexOf('function t2') >= 0, 'ccwdt.body-named');
chk(Array.isArray(ccwdt.dependencies), 'ccwdt.dependencies-array');

const cfcSrc = pug.compileFileClient(FP('partial.pug'));
chk(typeof cfcSrc === 'string' && cfcSrc.indexOf('function template') >= 0, 'compileFileClient.default-name');
chk(pug.compileFileClient(FP('partial.pug'), { name: 'myT' }).indexOf('function myT') >= 0, 'compileFileClient.named');

// Verify generated client function actually runs to the right HTML.
const clientFn = new Function(ccSrc + '\nreturn fnTpl;')();
chk(clientFn({ name: 'Z' }) === '<p>Hello Z</p>', 'compileClient.generated-runs');

// ===========================================================================
// 13. renderFile / compileFile / __express / basedir / includes / extends
// ===========================================================================
chk(pug.renderFile(FP('child.pug')) ===
  '<html><head><title>Home</title></head><body><h1>Welcome</h1><footer>base</footer></body></html>',
  'inherit.extends-block-override');
chk(pug.renderFile(FP('childappend.pug')) ===
  '<html><head><title>Default</title></head><body><p>prepended</p><p>default content</p><p>appended</p><footer>base</footer></body></html>',
  'inherit.block-append-prepend');
chk(pug.renderFile(FP('withinclude.pug')) ===
  '<p>before include</p><p>partial-included</p><p>after include</p>',
  'include.pug-partial');
chk(pug.renderFile(FP('withrawtxt.pug')) ===
  '<div class="box">raw text line1\nraw text line2\n</div>',
  'include.raw-txt');

const cFile = pug.compileFile(FP('child.pug'));
chk(typeof cFile === 'function', 'compileFile.returns-fn');
chk(cFile() ===
  '<html><head><title>Home</title></head><body><h1>Welcome</h1><footer>base</footer></body></html>',
  'compileFile.renders');

// render with filename + basedir resolves absolute extends "/layout"
chk(pug.render('extends /layout\nblock content\n  p via-basedir',
  { filename: FP('x.pug'), basedir: TDIR }) ===
  '<html><head><title>Default</title></head><body><p>via-basedir</p><footer>base</footer></body></html>',
  'inherit.basedir-absolute-extends');

// render callback (synchronous) form
let renderCb = null;
pug.render('p cb', {}, function (err, html) { renderCb = err ? ('ERR:' + err.message) : html; });
chk(renderCb === '<p>cb</p>', 'render.callback-form');

// renderFile callback form
let renderFileCb = null;
pug.renderFile(FP('partial.pug'), {}, function (err, html) { renderFileCb = err ? ('ERR:' + err.message) : html; });
chk(renderFileCb === '<p>partial-included</p>', 'renderFile.callback-form');

// __express(path, options, fn) — Express view-engine entry point
let expressCb = null;
pug.__express(FP('partial.pug'), {}, function (err, html) { expressCb = err ? ('ERR:' + err.message) : html; });
chk(expressCb === '<p>partial-included</p>', 'express.__express-entry');

// ===========================================================================
// FINAL RESULT
// ===========================================================================
console.log('PUG_RESULT ok=' + ok + ' fail=' + fail);
if (fail === 0) console.log('PUG_DONE');
process.exit(fail === 0 ? 0 : 1);
