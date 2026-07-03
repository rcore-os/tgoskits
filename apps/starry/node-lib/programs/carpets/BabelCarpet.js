'use strict';
// INDUSTRIAL carpet for @babel/core 7.26.0 (+ @babel/preset-typescript 7.26.0,
// @babel/preset-react 7.26.3). Host-verified on Node.js v22.22.2.
// All golden values were observed by RUNNING this exact toolchain, then baked
// in as exact-value (===) assertions. Deterministic: no clock, no randomness,
// no network. require() resolves from the cwd's node_modules.

const path = require('path');
const babel = require('@babel/core');
const t = babel.types;

let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }

// exact transform helper
function tx(code, opts) { return babel.transformSync(code, opts).code; }
function eq(code, opts, expected, name) { chk(tx(code, opts) === expected, name); }

const TS = { presets: [['@babel/preset-typescript']], filename: 'a.ts' };
const TSX = { presets: [['@babel/preset-typescript'], ['@babel/preset-react']], filename: 'a.tsx' };
const RJSX = { presets: [['@babel/preset-react']], filename: 'a.jsx' };

// ---------------------------------------------------------------------------
// 0. version / API surface
// ---------------------------------------------------------------------------
chk(babel.version === '7.26.0', 'version-7.26.0');
chk(typeof babel.transformSync === 'function', 'api-transformSync');
chk(typeof babel.transform === 'function', 'api-transform');
chk(typeof babel.transformAsync === 'function', 'api-transformAsync');
chk(typeof babel.parseSync === 'function', 'api-parseSync');
chk(typeof babel.parse === 'function', 'api-parse');
chk(typeof babel.parseAsync === 'function', 'api-parseAsync');
chk(typeof babel.transformFromAstSync === 'function', 'api-transformFromAstSync');
chk(typeof babel.transformFromAst === 'function', 'api-transformFromAst');
chk(typeof babel.traverse === 'function', 'api-traverse');
chk(typeof babel.template === 'function', 'api-template');
chk(typeof babel.types === 'object', 'api-types');
chk(typeof babel.loadOptions === 'function', 'api-loadOptions');
chk(typeof babel.loadPartialConfig === 'function', 'api-loadPartialConfig');
chk(typeof babel.createConfigItem === 'function', 'api-createConfigItem');
chk(Array.isArray(babel.DEFAULT_EXTENSIONS), 'api-DEFAULT_EXTENSIONS-array');
chk(babel.DEFAULT_EXTENSIONS.join(',') === '.js,.jsx,.es6,.es,.mjs,.cjs', 'DEFAULT_EXTENSIONS-value');
chk(babel.DEFAULT_EXTENSIONS.indexOf('.jsx') === 1, 'DEFAULT_EXTENSIONS-jsx');

// ---------------------------------------------------------------------------
// 1. @babel/preset-typescript — strip TS, lower enum/namespace/param-props
// ---------------------------------------------------------------------------
eq('const x: number = 1;', TS, 'const x = 1;', 'ts-type-annotation');
eq('interface Foo { a: number; }\nconst y = 2;', TS, 'const y = 2;', 'ts-interface-erased');
eq('type T = keyof Foo;\nconst a = 1;', TS, 'const a = 1;', 'ts-type-alias-erased');
eq('const v = x as string;', TS, 'const v = x;', 'ts-as-cast');
eq('const x = <number>y;', TS, 'const x = y;', 'ts-angle-cast');
eq('const w = { a: 1 } satisfies Record<string, number>;', TS,
   'const w = {\n  a: 1\n};', 'ts-satisfies');
eq('function id<T>(x: T): T { return x; }', TS,
   'function id(x) {\n  return x;\n}', 'ts-generic-function');
eq('const x = f<number>();', TS, 'const x = f();', 'ts-type-args-call');
eq('import type { Foo } from "./foo";\nconst z = 1;', TS,
   'const z = 1;\nexport {};', 'ts-import-type-only');
eq('declare const x: number;\nconst y = 1;', TS, 'const y = 1;', 'ts-declare-erased');
eq('const a = b!;', TS, 'const a = b;', 'ts-non-null');
eq('function f(a?: number) { return a; }', TS,
   'function f(a) {\n  return a;\n}', 'ts-optional-param');
eq('class C { readonly x: number = 1; }', TS,
   'class C {\n  x = 1;\n}', 'ts-readonly-field');
eq('abstract class C { abstract f(): void; g(){} }', TS,
   'class C {\n  g() {}\n}', 'ts-abstract-class');
eq('class C implements I { x = 1; }', TS,
   'class C {\n  x = 1;\n}', 'ts-implements-erased');
eq('import x = require("y");', TS, '', 'ts-import-equals-erased');
eq('class C { constructor(private x: number) {} }', TS,
   'class C {\n  constructor(x) {\n    this.x = x;\n  }\n}', 'ts-param-property');
// numeric enum lowering
eq('enum Color { Red, Green, Blue }', TS,
   'var Color = /*#__PURE__*/function (Color) {\n' +
   '  Color[Color["Red"] = 0] = "Red";\n' +
   '  Color[Color["Green"] = 1] = "Green";\n' +
   '  Color[Color["Blue"] = 2] = "Blue";\n' +
   '  return Color;\n}(Color || {});', 'ts-enum-numeric');
// const enum lowering (same shape on babel — no TS type-checker)
eq('const enum E { A, B }', TS,
   'var E = /*#__PURE__*/function (E) {\n' +
   '  E[E["A"] = 0] = "A";\n' +
   '  E[E["B"] = 1] = "B";\n' +
   '  return E;\n}(E || {});', 'ts-const-enum');
// string enum lowering
eq('enum E { A = "a", B = "b" }', TS,
   'var E = /*#__PURE__*/function (E) {\n' +
   '  E["A"] = "a";\n' +
   '  E["B"] = "b";\n' +
   '  return E;\n}(E || {});', 'ts-enum-string');
// namespace lowering
eq('namespace N { export const a = 1; }', TS,
   'let N;\n(function (_N) {\n  const a = _N.a = 1;\n})(N || (N = {}));', 'ts-namespace');
// bare-string preset form also works
eq('const x: number = 1;', { presets: ['@babel/preset-typescript'], filename: 'a.ts' },
   'const x = 1;', 'ts-bare-string-preset');

// ---------------------------------------------------------------------------
// 2. @babel/preset-react — JSX, classic vs automatic runtime
// ---------------------------------------------------------------------------
eq('const a = <div className="x">hi</div>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("div", {\n  className: "x"\n}, "hi");',
   'jsx-classic-element');
eq('const a = <>hi</>;', RJSX,
   'const a = /*#__PURE__*/React.createElement(React.Fragment, null, "hi");',
   'jsx-classic-fragment');
eq('const a = <Comp {...props} />;', RJSX,
   'const a = /*#__PURE__*/React.createElement(Comp, props);', 'jsx-spread-only');
eq('const a = <div>{value}</div>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("div", null, value);', 'jsx-expression-child');
eq('const a = <ul><li>1</li><li>2</li></ul>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("ul", null, ' +
   '/*#__PURE__*/React.createElement("li", null, "1"), ' +
   '/*#__PURE__*/React.createElement("li", null, "2"));', 'jsx-nested-classic');
eq('const a = <div>{cond ? <A/> : <B/>}</div>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("div", null, cond ? ' +
   '/*#__PURE__*/React.createElement(A, null) : ' +
   '/*#__PURE__*/React.createElement(B, null));', 'jsx-conditional');
eq('const a = <input disabled />;', RJSX,
   'const a = /*#__PURE__*/React.createElement("input", {\n  disabled: true\n});',
   'jsx-boolean-attr');
eq('const a = <Foo.Bar />;', RJSX,
   'const a = /*#__PURE__*/React.createElement(Foo.Bar, null);', 'jsx-member-expr');
eq('const a = <div>\n  hello\n  world\n</div>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("div", null, "hello world");',
   'jsx-whitespace-collapse');
eq('const a = <div>{/* c */}</div>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("div", null);', 'jsx-comment-child-dropped');
eq('const a = <div>&amp;</div>;', RJSX,
   'const a = /*#__PURE__*/React.createElement("div", null, "&");', 'jsx-html-entity');
// custom pragma (classic): no PURE annotation, custom factory
eq('const a = <div/>;', { presets: [['@babel/preset-react', { pragma: 'h', pragmaFrag: 'Fragment' }]], filename: 'a.jsx' },
   'const a = h("div", null);', 'jsx-custom-pragma');
// automatic runtime
eq('const a = <div className="x">hi</div>;', { presets: [['@babel/preset-react', { runtime: 'automatic' }]], filename: 'a.jsx' },
   'import { jsx as _jsx } from "react/jsx-runtime";\n' +
   'const a = /*#__PURE__*/_jsx("div", {\n  className: "x",\n  children: "hi"\n});',
   'jsx-automatic-jsx');
eq('const a = <>hi</>;', { presets: [['@babel/preset-react', { runtime: 'automatic' }]], filename: 'a.jsx' },
   'import { Fragment as _Fragment, jsx as _jsx } from "react/jsx-runtime";\n' +
   'const a = /*#__PURE__*/_jsx(_Fragment, {\n  children: "hi"\n});',
   'jsx-automatic-fragment');
eq('const a = <ul><li/><li/></ul>;', { presets: [['@babel/preset-react', { runtime: 'automatic' }]], filename: 'a.jsx' },
   'import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";\n' +
   'const a = /*#__PURE__*/_jsxs("ul", {\n  children: [/*#__PURE__*/_jsx("li", {}), /*#__PURE__*/_jsx("li", {})]\n});',
   'jsx-automatic-jsxs-multi');
eq('const a = <li key="1">x</li>;', { presets: [['@babel/preset-react', { runtime: 'automatic' }]], filename: 'a.jsx' },
   'import { jsx as _jsx } from "react/jsx-runtime";\n' +
   'const a = /*#__PURE__*/_jsx("li", {\n  children: "x"\n}, "1");',
   'jsx-automatic-key-arg');
eq('const a = <div/>;', { presets: [['@babel/preset-react', { runtime: 'automatic', importSource: 'preact' }]], filename: 'a.jsx' },
   'import { jsx as _jsx } from "preact/jsx-runtime";\nconst a = _jsx("div", {});',
   'jsx-automatic-importSource');

// ---------------------------------------------------------------------------
// 3. TSX — preset-typescript + preset-react together on a .tsx file
// ---------------------------------------------------------------------------
eq('const el = <div>{(x as number)}</div>;', TSX,
   'const el = /*#__PURE__*/React.createElement("div", null, x);', 'tsx-combined');

// ---------------------------------------------------------------------------
// 4. custom plugin (visitor) — Identifier rename, asserts visitor API
// ---------------------------------------------------------------------------
function renameFooToBar() {
  return { visitor: { Identifier(p) { if (p.node.name === 'foo') p.node.name = 'bar'; } } };
}
eq('const foo = 1; foo + foo;', { plugins: [renameFooToBar] },
   'const bar = 1;\nbar + bar;', 'plugin-identifier-rename');

// plugin that counts nodes via a closure (verifies path.node access)
let visited = 0;
function countNumericLiterals() {
  return { visitor: { NumericLiteral() { visited++; } } };
}
babel.transformSync('const a = 1 + 2 + 3;', { plugins: [countNumericLiterals] });
chk(visited === 3, 'plugin-visitor-count');

// ---------------------------------------------------------------------------
// 5. babel.parseSync — AST node types
// ---------------------------------------------------------------------------
const ast = babel.parseSync('const x = 1 + 2;');
chk(ast.type === 'File', 'parse-File');
chk(ast.program.type === 'Program', 'parse-Program');
chk(ast.program.sourceType === 'module', 'parse-sourceType-module');
chk(ast.program.body[0].type === 'VariableDeclaration', 'parse-VariableDeclaration');
chk(ast.program.body[0].kind === 'const', 'parse-decl-kind-const');
const decl0 = ast.program.body[0].declarations[0];
chk(decl0.type === 'VariableDeclarator', 'parse-VariableDeclarator');
chk(decl0.id.type === 'Identifier' && decl0.id.name === 'x', 'parse-declarator-id');
chk(decl0.init.type === 'BinaryExpression', 'parse-BinaryExpression');
chk(decl0.init.operator === '+', 'parse-binary-operator');
chk(decl0.init.left.type === 'NumericLiteral' && decl0.init.left.value === 1, 'parse-numeric-left');
chk(decl0.init.right.value === 2, 'parse-numeric-right');
// parse JSX requires the react preset's syntax
const jsxAst = babel.parseSync('const a = <div/>;', RJSX);
chk(jsxAst.program.body[0].declarations[0].init.type === 'JSXElement', 'parse-JSXElement');

// ---------------------------------------------------------------------------
// 6. babel.transformFromAstSync — round-trip + plugin over a pre-parsed AST
// ---------------------------------------------------------------------------
const rtAst = babel.parseSync('const y = 5;');
chk(babel.transformFromAstSync(rtAst, 'const y = 5;', {}).code === 'const y = 5;',
    'fromAst-roundtrip');
const mutAst = babel.parseSync('const foo = 1;');
chk(babel.transformFromAstSync(mutAst, 'const foo = 1;',
      { plugins: [function () { return { visitor: { Identifier(p) { if (p.node.name === 'foo') p.node.name = 'renamed'; } } }; }] }).code
    === 'const renamed = 1;', 'fromAst-plugin');

// ---------------------------------------------------------------------------
// 7. generator / output options
// ---------------------------------------------------------------------------
eq('const a = 1;\nconst b = 2;', { compact: true }, 'const a=1;const b=2;', 'opt-compact');
eq('const a = 1;\nconst b = 2;', { minified: true }, 'const a=1;const b=2;', 'opt-minified');
eq('const a = 1; // hi\nconst b = 2;', { comments: false }, 'const a = 1;\nconst b = 2;', 'opt-comments-false');
eq('const a = 1; // hi\nconst b = 2;', {}, 'const a = 1; // hi\nconst b = 2;', 'opt-comments-default');
eq('const a = 1; // keep\nconst b = 2; /* drop */', { shouldPrintComment: function (v) { return v.indexOf('keep') >= 0; } },
   'const a = 1; // keep\nconst b = 2;', 'opt-shouldPrintComment');
eq('const a = 1; // c\nfoo();', { compact: true }, 'const a=1;// c\nfoo();', 'opt-compact-keeps-comment');
eq('const a = 1;\n\n\nconst b = 2;', { retainLines: true }, 'const a = 1;\n\n\nconst b = 2;', 'opt-retainLines');
eq('const a =\n  1;', { retainLines: true }, 'const a =\n1;', 'opt-retainLines-2');
eq('var a = 1;', { sourceType: 'script' }, 'var a = 1;', 'opt-sourceType-script');
eq('export const a = 1;', { sourceType: 'module' }, 'export const a = 1;', 'opt-sourceType-module');
eq('return 1;', { parserOpts: { allowReturnOutsideFunction: true } }, 'return 1;', 'opt-parserOpts');

// transform result metadata shape
const res = babel.transformSync('const a = 1;', { ast: true, sourceMaps: true });
chk(typeof res.code === 'string', 'result-code-string');
chk(res.ast && res.ast.type === 'File', 'result-ast-File');
chk(res.map != null && Array.isArray(res.map.sources), 'result-sourcemap');
chk(res.sourceType === 'module', 'result-sourceType');

// ---------------------------------------------------------------------------
// 8. code-frame error on syntax error (loc + caret), portable assertions
// ---------------------------------------------------------------------------
let threw = false, err = null;
try { babel.transformSync('const = ;', { filename: 'bad.js' }); }
catch (e) { threw = true; err = e; }
chk(threw === true, 'err-thrown');
chk(err && err.code === 'BABEL_PARSE_ERROR', 'err-code');
chk(err && err.loc && err.loc.line === 1 && err.loc.column === 6, 'err-loc');
chk(err && err.reasonCode === 'UnexpectedToken', 'err-reasonCode');
chk(err && err.message.indexOf('Unexpected token') >= 0, 'err-message-text');
chk(err && err.message.indexOf('(1:6)') >= 0, 'err-message-position');
chk(err && err.message.indexOf('^') >= 0, 'err-codeframe-caret');
// TS requires the right filename: .js input rejects type annotations
let tsThrew = false;
try { babel.transformSync('const x: number = 1;', { filename: 'plain.js' }); }
catch (e) { tsThrew = true; }
chk(tsThrew === true, 'err-ts-needs-ts-filename');

// ---------------------------------------------------------------------------
// 9. @babel/types — node builders / predicates
// ---------------------------------------------------------------------------
chk(typeof t.isIdentifier === 'function', 'types-isIdentifier-fn');
const idNode = t.identifier('xyz');
chk(idNode.type === 'Identifier' && idNode.name === 'xyz', 'types-identifier-build');
chk(t.isIdentifier(idNode) === true, 'types-isIdentifier-true');
chk(t.isIdentifier(t.numericLiteral(1)) === false, 'types-isIdentifier-false');
const numNode = t.numericLiteral(42);
chk(numNode.type === 'NumericLiteral' && numNode.value === 42, 'types-numericLiteral');
chk(t.isNumericLiteral(numNode, { value: 42 }) === true, 'types-isNumericLiteral-shape');

// ---------------------------------------------------------------------------
// 10. @babel/template — build AST from template, generate code back
// ---------------------------------------------------------------------------
const buildConst = babel.template('const %%name%% = %%val%%;');
const tmplNode = buildConst({ name: t.identifier('z'), val: t.numericLiteral(42) });
chk(tmplNode.type === 'VariableDeclaration', 'template-node-type');
chk(babel.transformFromAstSync(t.file(t.program([tmplNode])), '', {}).code === 'const z = 42;',
    'template-generate');
const stmtTmpl = babel.template.statement('const A = B;');
const stmtNode = stmtTmpl({ A: t.identifier('q'), B: t.numericLiteral(7) });
chk(stmtNode.type === 'VariableDeclaration', 'template-statement-type');
chk(babel.transformFromAstSync(t.file(t.program([stmtNode])), '', {}).code === 'const q = 7;',
    'template-statement-generate');
const exprTmpl = babel.template.expression('1 + 2');
chk(exprTmpl().type === 'BinaryExpression', 'template-expression-type');

// ---------------------------------------------------------------------------
// 11. babel.traverse — walk a parsed AST
// ---------------------------------------------------------------------------
let idCount = 0;
babel.traverse(babel.parseSync('const a = 1; const b = 2; a + b;'),
  { Identifier() { idCount++; } });
chk(idCount === 4, 'traverse-identifier-count');
let binCount = 0;
babel.traverse(babel.parseSync('a + b - c;'), { BinaryExpression() { binCount++; } });
chk(binCount === 2, 'traverse-binary-count');

// ---------------------------------------------------------------------------
// 12. config loading helpers
// ---------------------------------------------------------------------------
const loaded = babel.loadOptions({ presets: [['@babel/preset-react']], filename: 'a.jsx', configFile: false, babelrc: false });
chk(Array.isArray(loaded.plugins), 'loadOptions-plugins-array');
chk(loaded.plugins.length > 0, 'loadOptions-plugins-nonempty');
const partial = babel.loadPartialConfig({ filename: 'a.js', configFile: false, babelrc: false });
chk(!!partial.options, 'loadPartialConfig-options');
const item = babel.createConfigItem(['@babel/preset-react', {}], { type: 'preset' });
chk(typeof item.value === 'function', 'createConfigItem-value');

// ---------------------------------------------------------------------------
// cwd option: babel honours an explicit cwd + relative filename, portably
// ---------------------------------------------------------------------------
chk(babel.transformSync('const x = 1;', { cwd: __dirname, filename: 'a.ts', configFile: false, babelrc: false, presets: ['@babel/preset-typescript'] }).code === 'const x = 1;', 'babel-cwd-option');

// ---------------------------------------------------------------------------
console.log('BABEL_RESULT ok=' + ok + ' fail=' + fail);
if (fail === 0) console.log('BABEL_DONE');
process.exit(fail === 0 ? 0 : 1);
