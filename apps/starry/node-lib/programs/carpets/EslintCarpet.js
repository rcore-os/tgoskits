'use strict';
// INDUSTRIAL carpet for eslint 9.18.0  (Linter + ESLint flat-config, in-process, deterministic)
// Ground truth = observed behaviour of eslint@9.18.0 on Node.js v22.22.2.
// No fs writes, no CLI, no network, no clock/random. Pure require('eslint').
const path = require('path');
const { Linter, ESLint, SourceCode } = require('eslint');

let ok = 0, fail = 0;
function chk(cond, name) { if (cond) ok++; else { fail++; console.log('FAIL ' + name); } }

const L = new Linter();

// Convenience: lint and return the message array.
function lint(code, config) { return L.verify(code, config); }
// Assert exact diagnostic fields of one message.
function dia(m, exp, tag) {
  chk(m !== undefined, tag + '/exists');
  if (m === undefined) return;
  if ('ruleId' in exp) chk(m.ruleId === exp.ruleId, tag + '/ruleId(' + m.ruleId + ')');
  if ('messageId' in exp) chk(m.messageId === exp.messageId, tag + '/messageId(' + m.messageId + ')');
  if ('severity' in exp) chk(m.severity === exp.severity, tag + '/severity(' + m.severity + ')');
  if ('line' in exp) chk(m.line === exp.line, tag + '/line(' + m.line + ')');
  if ('column' in exp) chk(m.column === exp.column, tag + '/column(' + m.column + ')');
  if ('endLine' in exp) chk(m.endLine === exp.endLine, tag + '/endLine(' + m.endLine + ')');
  if ('endColumn' in exp) chk(m.endColumn === exp.endColumn, tag + '/endColumn(' + m.endColumn + ')');
  if ('message' in exp) chk(m.message === exp.message, tag + '/message(' + JSON.stringify(m.message) + ')');
}

// ---------------------------------------------------------------------------
// 0. Static version surface
// ---------------------------------------------------------------------------
chk(Linter.version === '9.18.0', 'Linter.version');
chk(ESLint.version === '9.18.0', 'ESLint.version');
chk(typeof L.verify === 'function', 'Linter.verify is fn');
chk(typeof L.verifyAndFix === 'function', 'Linter.verifyAndFix is fn');

// ---------------------------------------------------------------------------
// 1. Core rules — exact ruleId/messageId/severity/position/message
// ---------------------------------------------------------------------------

// semi (note: missingSemi has no endLine/endColumn)
{
  const m = lint('var x = 1', { rules: { semi: 'error' } });
  chk(m.length === 1, 'semi/count');
  dia(m[0], { ruleId: 'semi', messageId: 'missingSemi', severity: 2, line: 1, column: 10, message: 'Missing semicolon.' }, 'semi');
  chk(m[0].endLine === undefined, 'semi/endLine-undef');
  chk(m[0].endColumn === undefined, 'semi/endColumn-undef');
  chk(m[0].fix && m[0].fix.text === ';', 'semi/fix-text');
  chk(m[0].fix.range[0] === 9 && m[0].fix.range[1] === 9, 'semi/fix-range');
}

// no-var
dia(lint('var x = 1;', { rules: { 'no-var': 'error' } })[0],
  { ruleId: 'no-var', messageId: 'unexpectedVar', severity: 2, line: 1, column: 1, endLine: 1, endColumn: 11, message: 'Unexpected var, use let or const instead.' }, 'no-var');

// quotes (default double -> single)
dia(lint('var x = "hi";', { rules: { quotes: ['error', 'single'] } })[0],
  { ruleId: 'quotes', messageId: 'wrongQuotes', severity: 2, line: 1, column: 9, endLine: 1, endColumn: 13, message: 'Strings must use singlequote.' }, 'quotes');

// eqeqeq
dia(lint('if (a == b) {}', { rules: { eqeqeq: 'error' }, languageOptions: { globals: { a: 'readonly', b: 'readonly' } } })[0],
  { ruleId: 'eqeqeq', messageId: 'unexpected', severity: 2, line: 1, column: 7, endLine: 1, endColumn: 9, message: "Expected '===' and instead saw '=='." }, 'eqeqeq');

// no-unused-vars (two messages: unused function 'f', unused var 'y')
{
  const m = lint('function f(){ var y = 2; }', { rules: { 'no-unused-vars': 'warn' } });
  chk(m.length === 2, 'no-unused-vars/count');
  dia(m[0], { ruleId: 'no-unused-vars', messageId: 'unusedVar', severity: 1, line: 1, column: 10, endLine: 1, endColumn: 11, message: "'f' is defined but never used." }, 'unused/f');
  dia(m[1], { ruleId: 'no-unused-vars', messageId: 'unusedVar', severity: 1, line: 1, column: 19, endLine: 1, endColumn: 20, message: "'y' is assigned a value but never used." }, 'unused/y');
}

// no-undef
dia(lint('foo();', { rules: { 'no-undef': 'error' } })[0],
  { ruleId: 'no-undef', messageId: 'undef', severity: 2, line: 1, column: 1, endLine: 1, endColumn: 4, message: "'foo' is not defined." }, 'no-undef');

// prefer-const
dia(lint('let z = 5; console.log(z);', { rules: { 'prefer-const': 'error' }, languageOptions: { globals: { console: 'readonly' } } })[0],
  { ruleId: 'prefer-const', messageId: 'useConst', severity: 2, line: 1, column: 5, endLine: 1, endColumn: 6, message: "'z' is never reassigned. Use 'const' instead." }, 'prefer-const');

// no-console (warn)
dia(lint('console.log(1);', { rules: { 'no-console': 'warn' }, languageOptions: { globals: { console: 'readonly' } } })[0],
  { ruleId: 'no-console', messageId: 'unexpected', severity: 1, line: 1, column: 1, endLine: 1, endColumn: 12, message: 'Unexpected console statement.' }, 'no-console');

// curly
dia(lint('if (a) b();', { rules: { curly: 'error' }, languageOptions: { globals: { a: 'readonly', b: 'readonly' } } })[0],
  { ruleId: 'curly', messageId: 'missingCurlyAfterCondition', severity: 2, line: 1, column: 8, endLine: 1, endColumn: 12, message: "Expected { after 'if' condition." }, 'curly');

// indent (second line should be 2 spaces, is 0)
dia(lint('function f() {\nreturn 1;\n}', { rules: { indent: ['error', 2] } })[0],
  { ruleId: 'indent', messageId: 'wrongIndentation', severity: 2, line: 2, column: 1, endLine: 2, endColumn: 1, message: 'Expected indentation of 2 spaces but found 0.' }, 'indent');

// no-multi-spaces
dia(lint('var x =  1;', { rules: { 'no-multi-spaces': 'error' } })[0],
  { ruleId: 'no-multi-spaces', messageId: 'multipleSpaces', severity: 2, line: 1, column: 8, endLine: 1, endColumn: 10, message: "Multiple spaces found before '1'." }, 'no-multi-spaces');

// no-dupe-keys
dia(lint('var o = { a: 1, a: 2 };', { rules: { 'no-dupe-keys': 'error' } })[0],
  { ruleId: 'no-dupe-keys', messageId: 'unexpected', severity: 2, line: 1, column: 17, endLine: 1, endColumn: 18, message: "Duplicate key 'a'." }, 'no-dupe-keys');

// no-unreachable
dia(lint('function f(){ return 1; foo(); }', { rules: { 'no-unreachable': 'error' } })[0],
  { ruleId: 'no-unreachable', messageId: 'unreachableCode', severity: 2, line: 1, column: 25, endLine: 1, endColumn: 31, message: 'Unreachable code.' }, 'no-unreachable');

// no-constant-condition
dia(lint('if (true) {}', { rules: { 'no-constant-condition': 'error' } })[0],
  { ruleId: 'no-constant-condition', messageId: 'unexpected', severity: 2, line: 1, column: 5, endLine: 1, endColumn: 9, message: 'Unexpected constant condition.' }, 'no-constant-condition');

// complexity (array option [error, 1] -> reports complexity 3)
dia(lint('function f(a){ if(a){} else {} if(a){} return a; }', { rules: { complexity: ['error', 1] } })[0],
  { ruleId: 'complexity', messageId: 'complex', severity: 2, line: 1, column: 1, endLine: 1, endColumn: 51, message: "Function 'f' has a complexity of 3. Maximum allowed is 1." }, 'complexity');

// max-len (array option [error, 20])
dia(lint('var aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1;', { rules: { 'max-len': ['error', 20] } })[0],
  { ruleId: 'max-len', messageId: 'max', severity: 2, line: 1, column: 1, endLine: 1, endColumn: 51, message: 'This line has a length of 50. Maximum allowed is 20.' }, 'max-len');

// ---------------------------------------------------------------------------
// 2. Additional core rules (broaden the carpet)
// ---------------------------------------------------------------------------
dia(lint('if (x) {}', { rules: { 'no-empty': 'error' }, languageOptions: { globals: { x: 'readonly' } } })[0],
  { ruleId: 'no-empty', messageId: 'unexpected', severity: 2, line: 1, column: 8, endLine: 1, endColumn: 10, message: 'Empty block statement.' }, 'no-empty');

dia(lint('var x = 1;;', { rules: { 'no-extra-semi': 'error' } })[0],
  { ruleId: 'no-extra-semi', messageId: 'unexpected', severity: 2, line: 1, column: 11, endLine: 1, endColumn: 12, message: 'Unnecessary semicolon.' }, 'no-extra-semi');

dia(lint('var a=1; var a=2;', { rules: { 'no-redeclare': 'error' } })[0],
  { ruleId: 'no-redeclare', messageId: 'redeclared', severity: 2, line: 1, column: 14, endLine: 1, endColumn: 15, message: "'a' is already defined." }, 'no-redeclare');

dia(lint('if (x === NaN) {}', { rules: { 'use-isnan': 'error' }, languageOptions: { globals: { x: 'readonly', NaN: 'readonly' } } })[0],
  { ruleId: 'use-isnan', messageId: 'comparisonWithNaN', severity: 2, line: 1, column: 5, endLine: 1, endColumn: 14, message: 'Use the isNaN function to compare with NaN.' }, 'use-isnan');

dia(lint('if (typeof x === "strnig") {}', { rules: { 'valid-typeof': 'error' }, languageOptions: { globals: { x: 'readonly' } } })[0],
  { ruleId: 'valid-typeof', messageId: 'invalidValue', severity: 2, line: 1, column: 18, endLine: 1, endColumn: 26, message: 'Invalid typeof comparison value.' }, 'valid-typeof');

dia(lint('if (x = 1) {}', { rules: { 'no-cond-assign': 'error' }, languageOptions: { globals: { x: 'writable' } } })[0],
  { ruleId: 'no-cond-assign', messageId: 'missing', severity: 2, line: 1, column: 5, endLine: 1, endColumn: 10, message: 'Expected a conditional expression and instead saw an assignment.' }, 'no-cond-assign');

dia(lint('var o = {}; o["a"];', { rules: { 'dot-notation': 'error' } })[0],
  { ruleId: 'dot-notation', messageId: 'useDot', severity: 2, line: 1, column: 15, endLine: 1, endColumn: 18, message: '["a"] is better written in dot notation.' }, 'dot-notation');

dia(lint('function f(a, a) {}', { rules: { 'no-dupe-args': 'error' }, languageOptions: { sourceType: 'script' } })[0],
  { ruleId: 'no-dupe-args', messageId: 'unexpected', severity: 2, line: 1, column: 1, endLine: 1, endColumn: 20, message: "Duplicate param 'a'." }, 'no-dupe-args');

dia(lint('function f(){} f = 1;', { rules: { 'no-func-assign': 'error' } })[0],
  { ruleId: 'no-func-assign', messageId: 'isAFunction', severity: 2, line: 1, column: 16, endLine: 1, endColumn: 17, message: "'f' is a function." }, 'no-func-assign');

dia(lint('debugger;', { rules: { 'no-debugger': 'error' } })[0],
  { ruleId: 'no-debugger', messageId: 'unexpected', severity: 2, line: 1, column: 1, endLine: 1, endColumn: 10, message: "Unexpected 'debugger' statement." }, 'no-debugger');

dia(lint('[].map(function(x){return x;});', { rules: { 'prefer-arrow-callback': 'error' } })[0],
  { ruleId: 'prefer-arrow-callback', messageId: 'preferArrowCallback', severity: 2, line: 1, column: 8, endLine: 1, endColumn: 30, message: 'Unexpected function expression.' }, 'prefer-arrow-callback');

// ---------------------------------------------------------------------------
// 3. languageOptions: ecmaVersion / sourceType / globals
// ---------------------------------------------------------------------------
// module: import OK -> zero messages
chk(lint('import x from "y"; x();', { languageOptions: { ecmaVersion: 2022, sourceType: 'module' }, rules: {} }).length === 0, 'module-import-clean');

// script: import is a fatal parse error
{
  const m = lint('import x from "y";', { languageOptions: { ecmaVersion: 2022, sourceType: 'script' }, rules: {} });
  chk(m.length === 1, 'script-import/count');
  chk(m[0].ruleId === null, 'script-import/ruleId-null');
  chk(m[0].fatal === true, 'script-import/fatal');
  chk(m[0].severity === 2, 'script-import/severity');
  chk(m[0].line === 1 && m[0].column === 1, 'script-import/pos');
  chk(m[0].message === "Parsing error: 'import' and 'export' may appear only with 'sourceType: module'", 'script-import/message');
}

// ecmaVersion 2022: private class fields parse cleanly
chk(lint('class A { #x = 1; m(){ return this.#x; } }', { languageOptions: { ecmaVersion: 2022, sourceType: 'script' }, rules: {} }).length === 0, 'ecma2022-private-clean');

// globals: declared readonly global is not flagged by no-undef
chk(lint('myGlobal();', { languageOptions: { globals: { myGlobal: 'readonly' } }, rules: { 'no-undef': 'error' } }).length === 0, 'globals-readonly-clean');

// numeric severities 2 (error) and 1 (warn) coexist; sorted by position
{
  const m = lint('var x=1', { rules: { semi: 2, 'no-unused-vars': 1 } });
  chk(m.length === 2, 'numeric-sev/count');
  chk(m[0].ruleId === 'no-unused-vars' && m[0].severity === 1, 'numeric-sev/warn-first');
  chk(m[1].ruleId === 'semi' && m[1].severity === 2, 'numeric-sev/error-second');
}

// ---------------------------------------------------------------------------
// 4. Rule options: [severity, primitive] and [severity, {object}]
// ---------------------------------------------------------------------------
dia(lint('var x = 1;', { rules: { semi: ['error', 'never'] } })[0],
  { ruleId: 'semi', messageId: 'extraSemi', severity: 2, line: 1, column: 10, message: 'Extra semicolon.' }, 'semi-never');

dia(lint('var x = "hi";', { rules: { quotes: ['error', 'backtick'] } })[0],
  { ruleId: 'quotes', messageId: 'wrongQuotes', severity: 2, message: 'Strings must use backtick.' }, 'quotes-backtick');

// no-unused-vars with {args:'none'} -> only the function is reported (1)
chk(lint('function f(a){ return 1; }', { rules: { 'no-unused-vars': ['warn', { args: 'none' }] } }).length === 1, 'unused/args-none-count');
// with {args:'all'} -> function + unused arg (2)
{
  const m = lint('function f(a){ return 1; }', { rules: { 'no-unused-vars': ['warn', { args: 'all' }] } });
  chk(m.length === 2, 'unused/args-all-count');
  chk(m[1].column === 12 && m[1].message === "'a' is defined but never used.", 'unused/args-all-arg');
}

// complexity object {max:1}
chk(lint('function f(a){ if(a){} if(a){} return a; }', { rules: { complexity: ['error', { max: 1 }] } })[0].message ===
  "Function 'f' has a complexity of 3. Maximum allowed is 1.", 'complexity-obj');

// max-len object {code:10}
chk(lint('var x = 1; // aaaaaaaaaaaaaaaaaaaaaaa', { rules: { 'max-len': ['error', { code: 10 }] } })[0].message ===
  'This line has a length of 37. Maximum allowed is 10.', 'max-len-obj');

// ---------------------------------------------------------------------------
// 5. verifyAndFix — exact fixed output (single + multi-pass)
// ---------------------------------------------------------------------------
{
  const r = L.verifyAndFix('var x = 1', { rules: { semi: 'error' } });
  chk(r.fixed === true, 'fix-semi/fixed');
  chk(r.output === 'var x = 1;', 'fix-semi/output');
  chk(r.messages.length === 0, 'fix-semi/no-msg');
}
chk(L.verifyAndFix('var x = "hi";', { rules: { quotes: ['error', 'single'] } }).output === "var x = 'hi';", 'fix-quotes/output');
chk(L.verifyAndFix('var x = 1;', { rules: { 'no-var': 'error' } }).output === 'let x = 1;', 'fix-novar/output');
// multi-pass: both no-var and semi applied
{
  const r = L.verifyAndFix('var x = 1', { rules: { semi: 'error', 'no-var': 'error' } });
  chk(r.fixed === true && r.output === 'let x = 1;', 'fix-multipass/output');
}

// ---------------------------------------------------------------------------
// 6. Custom inline rule via config.plugins
// ---------------------------------------------------------------------------
{
  const cfg = {
    plugins: {
      my: {
        rules: {
          'no-foo': {
            create(ctx) {
              return { Identifier(node) { if (node.name === 'foo') ctx.report({ node, message: 'no foo allowed' }); } };
            }
          }
        }
      }
    },
    rules: { 'my/no-foo': 'error' }
  };
  const m = lint('var foo = 1;', cfg);
  chk(m.length === 1, 'custom/count');
  dia(m[0], { ruleId: 'my/no-foo', severity: 2, line: 1, column: 5, message: 'no foo allowed' }, 'custom');
  // and does not fire when identifier absent
  chk(lint('var bar = 1;', cfg).length === 0, 'custom/no-fire');
}

// ---------------------------------------------------------------------------
// 7. Message ordering / sort stability across multiple rules & lines
// ---------------------------------------------------------------------------
{
  const code = 'var x = "a";\nvar y = "b"';
  const m = lint(code, { rules: { quotes: ['error', 'single'], semi: 'error', 'no-unused-vars': 'warn', 'no-var': 'error' } });
  chk(m.length === 7, 'sort/count');
  let sorted = true;
  for (let i = 1; i < m.length; i++) {
    if (m[i].line < m[i - 1].line || (m[i].line === m[i - 1].line && m[i].column < m[i - 1].column)) sorted = false;
  }
  chk(sorted === true, 'sort/by-line-col');
  chk(m[0].ruleId === 'no-var' && m[0].line === 1 && m[0].column === 1, 'sort/first');
  chk(m[m.length - 1].ruleId === 'semi' && m[m.length - 1].line === 2, 'sort/last');
}

// clean code -> zero messages
chk(lint('const x = 1;\nexport default x;', { languageOptions: { sourceType: 'module' }, rules: { semi: 'error', quotes: 'error', 'no-unused-vars': 'error' } }).length === 0, 'clean/zero');

// ---------------------------------------------------------------------------
// 8. Inline disable directives + getSuppressedMessages
// ---------------------------------------------------------------------------
{
  const active = lint('var x = 1 // eslint-disable-line semi', { rules: { semi: 'error' } });
  chk(active.length === 0, 'disable-line/active-zero');
  const sup = L.getSuppressedMessages();
  chk(sup.length === 1, 'disable-line/suppressed-count');
  chk(sup[0].ruleId === 'semi', 'disable-line/suppressed-ruleId');
  chk(sup[0].suppressions[0].kind === 'directive', 'disable-line/suppression-kind');
}

// ---------------------------------------------------------------------------
// 9. SourceCode access after verify
// ---------------------------------------------------------------------------
{
  L.verify('const a = 1;', { rules: {} });
  const sc = L.getSourceCode();
  chk(sc instanceof SourceCode, 'sourcecode/instanceof');
  chk(sc.getText() === 'const a = 1;', 'sourcecode/text');
  chk(sc.ast && sc.ast.type === 'Program', 'sourcecode/ast-program');
  chk(sc.ast.body.length === 1, 'sourcecode/ast-body');
}

// ---------------------------------------------------------------------------
// 10. ESLint class — overrideConfig + lintText (in-memory, no fs), fix, filePath
// ---------------------------------------------------------------------------
(async () => {
  try {
    const eslint = new ESLint({ overrideConfigFile: true, overrideConfig: { rules: { semi: 'error', 'no-var': 'error' } } });
    const results = await eslint.lintText('var x = 1');
    chk(Array.isArray(results) && results.length === 1, 'eslint/result-array');
    const r = results[0];
    chk(r.filePath === '<text>', 'eslint/filePath-text');
    chk(r.errorCount === 2, 'eslint/errorCount');
    chk(r.warningCount === 0, 'eslint/warningCount');
    chk(r.fixableErrorCount === 2, 'eslint/fixableErrorCount');
    chk(r.messages.length === 2, 'eslint/messages-count');
    chk(r.messages[0].ruleId === 'no-var' && r.messages[0].messageId === 'unexpectedVar', 'eslint/msg0');
    chk(r.messages[1].ruleId === 'semi' && r.messages[1].messageId === 'missingSemi', 'eslint/msg1');

    // clean text -> no errors (let, with semicolon -> neither rule fires)
    const clean = await eslint.lintText('let x = 1;');
    chk(clean[0].errorCount === 0 && clean[0].messages.length === 0, 'eslint/clean');

    // fix:true -> exact fixed output
    const eslintFix = new ESLint({ overrideConfigFile: true, fix: true, overrideConfig: { rules: { semi: 'error', 'no-var': 'error' } } });
    const fixed = await eslintFix.lintText('var x = 1');
    chk(fixed[0].output === 'let x = 1;', 'eslint/fix-output');

    // filePath option resolves against cwd (portable: derive expected from process.cwd())
    const wp = await eslint.lintText('var x = 1', { filePath: 'foo.js' });
    chk(wp[0].filePath === path.resolve(process.cwd(), 'foo.js'), 'eslint/filePath-resolved');
    chk(wp[0].errorCount === 2, 'eslint/filePath-errorCount');

    // ESLint.outputFixes is a static function (no fs side effect when output absent)
    chk(typeof ESLint.outputFixes === 'function', 'eslint/outputFixes-fn');
    chk(typeof eslint.calculateConfigForFile === 'function', 'eslint/calcConfig-fn');
  } catch (e) {
    fail++;
    console.log('FAIL eslint-class/exception ' + (e && e.message));
  }

  // ----- final tally -----
  console.log('ESLINT_RESULT ok=' + ok + ' fail=' + fail);
  if (fail === 0) console.log('ESLINT_DONE');
  process.exit(fail === 0 ? 0 : 1);
})();
