# node-lib — Node.js library carpet

Industrial-grade, on-target test of a set of common Node.js libraries, run by **Node.js 22**
(Alpine v3.22 LTS, full V8 JIT, no kernel change) on StarryOS across all four architectures
(x86_64 / aarch64 / riscv64 / loongarch64). The Node patch level is not pinned — `prebuild.sh`
resolves the current Alpine v3.22 `nodejs` (v22.23.0 at the time of writing).

Each module is a self-contained carpet that exercises one library's public API surface
(hundreds of exact-value assertions; CSS preprocessors and JS transforms are compared against
exact golden output computed from each library's documented behaviour at its pinned version —
the library versions are locked by `assets/package-lock.json`). A
module prints an anchored `*_DONE` marker only when its internal fail count is zero;
`run-nlib.sh` runs every module and emits `TEST PASSED` only when all of them pass (no skip).

## Run

```
cargo xtask starry app qemu -t node-lib --arch x86_64
cargo xtask starry app qemu -t node-lib --arch aarch64
cargo xtask starry app qemu -t node-lib --arch riscv64
cargo xtask starry app qemu -t node-lib --arch loongarch64
```

`prebuild.sh` `apk add`s the musl-native Node.js 22 + npm runtime into the per-app rootfs
`/usr`, then runs `npm ci --omit=optional` from the committed `assets/package.json` +
`assets/package-lock.json` to provision the library `node_modules` into `/root/nlib` (nothing
binary is committed — the dependency closure is fetched from the npm registry at build time and
injected into the overlay). A developer with a local npm cache can point `NODE_DL_ROOT` at it
(`--prefer-offline`, falls back to the network on a miss).

## Coverage

| module | library | dimension | marker |
|:--|:--|:--|:--|
| less | less 4.2.2 | render API + variables / arithmetic / nesting / mixins / guards / extend / maps / @import / functions / plugins → byte-exact CSS | `LESS_DONE` |
| stylus | stylus 0.64.0 | render + JS `define`/`set` API / mixins / conditionals / iteration / hashes / built-ins / @extend → byte-exact CSS | `STYLUS_DONE` |
| scss | sass 1.83.4 (Dart Sass) | compileString/compile + @mixin/@function / control flow / `sass:*` modules / @use / @extend / maps → exact CSS | `SCSS_DONE` |
| babel | @babel/core 7.26.0 | transformSync + preset-typescript (TS strip) + preset-react (JSX) + custom plugins / parse / AST round-trip | `BABEL_DONE` |
| terser | terser 5.37.0 | minify + mangle / compress / format / sourceMap / nameCache options → exact minified output | `TERSER_DONE` |
| eslint | eslint 9.18.0 | `Linter`/`ESLint` flat-config lint: exact diagnostics (ruleId / messageId / line / col / severity) + `verifyAndFix` | `ESLINT_DONE` |
| cjsesm | Node ESM/CJS | CommonJS↔ESM interop: require / dynamic import / data: URL / createRequire / circular resolution → exact values | `CJSESM_DONE` |

The carpet sources live in `programs/carpets/`; the libraries are declared in
`assets/package.json` + `assets/package-lock.json` and provisioned by `prebuild.sh`'s
`npm ci` (the `node_modules` closure is not committed to the source tree).
