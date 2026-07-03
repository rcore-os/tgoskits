# node-web — Node.js web-framework carpet

Industrial-grade, on-target test of a set of real Node.js web frameworks, run by **Node.js
22 LTS** (full V8 JIT, no kernel change) on StarryOS across all four architectures
(x86_64 / aarch64 / riscv64 / loongarch64).

Each module is a self-contained carpet that exercises one framework's public API surface
(hundreds of exact-value assertions). Every assertion compares against an exact golden value
computed from the framework's documented behaviour on this exact node + package version. A
module prints an anchored `*_DONE` marker only when its internal fail count is zero;
`run-nweb.sh` runs every module and emits `TEST PASSED` only when all of them pass (no skip).

This app commits **only source + manifests** — no dependency-closure `node_modules/` and no
generated `kotlin-app.js` are checked in. `prebuild.sh` provisions both reproducibly (see
`programs/SOURCES.md`).

## Run

```
cargo xtask starry app qemu -t node-web --arch x86_64
cargo xtask starry app qemu -t node-web --arch aarch64
cargo xtask starry app qemu -t node-web --arch riscv64
cargo xtask starry app qemu -t node-web --arch loongarch64
```

`prebuild.sh`:

1. `apk add nodejs npm icu-data-full` (Node 22 LTS, Alpine v3.22) into a staged rootfs via
   `qemu-user-static`, then copies node + its shared-library closure + ICU into the app overlay
   `/usr`. Offline fallback: `NODE_APK_CACHE`/`NODE_DL_ROOT` (optional).
2. `npm ci --omit=optional` from the committed `assets/package.json` + `assets/package-lock.json`
   to provision the pug/express `node_modules` closure into `/root/nweb` (nothing vendored). The
   closure is pure JS, so it is architecture-independent. Optional local npm cache via
   `NWEB_NPM_CACHE`/`NODE_DL_ROOT`.
3. Regenerates `kotlin-app.js` from the committed `assets/kotlin-app.kt` with the pinned Kotlin
   2.0.21 JS compiler (fetched by sha256, or a host `kotlinc-js`, or an optional pre-generated
   cache — see `programs/SOURCES.md`), staged into `/root/nweb/carpets`.

## Coverage

| module | framework | dimension | marker |
|:--|:--|:--|:--|
| pug | pug 3.0.3 | template engine: full API (render/renderFile/compile/compileFile/compileClient) + tags / attributes / interpolation / escaping / conditionals / iteration / mixins / inheritance / includes / filters / doctype | `PUG_DONE` |
| express | express 4.21.2 | web framework over real IPv4 loopback: routing / params / query / Router / middleware / body parsers / error handling / static / full response & request API | `EXPRESS_DONE` |
| kotlin-js | Kotlin/JS (Kotlin 2.0.21 IR → commonjs) | a Kotlin→JS module generated in prebuild from `assets/kotlin-app.kt` and run on node; stdout byte-identical to the golden, per-line + per-token assertions | `KOTLINJS_DONE` |

The carpet sources live in `programs/carpets/`. The pug/express libraries are provisioned by
`npm ci` from `assets/package.json` + `assets/package-lock.json`, and the Kotlin/JS module is
generated from `assets/kotlin-app.kt`; neither is committed. See `programs/SOURCES.md` for exact
versions, sha256 pins, and the Kotlin regeneration recipe.
