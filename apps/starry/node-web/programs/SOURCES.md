# node-web — asset provenance (source-only repo)

This app commits **only source + manifests**. No dependency-closure `node_modules/` and no
generated `kotlin-app.js` are checked in. `prebuild.sh` provisions both from committed source in
a reproducible, integrity-checked way and stages the result into the per-app rootfs. This file
records the provenance of every provisioned / generated asset.

## Node runtime (Alpine v3.22, apk)

`prebuild.sh` runs `apk add nodejs npm icu-data-full` (Node.js 22 LTS) into a staged copy of the
base Alpine rootfs via `qemu-user-static`, resolving the CURRENT v3.22 closure for the target
arch (no pinned/drifting apk URLs). node + its shared-library closure + ICU data are copied into
the app overlay. Offline fallback: a pre-fetched apk cache under `$NODE_APK_CACHE/<arch>/`
(default `<repo>/download/nodejs-apks/<arch>`) — OPTIONAL, network is the primary path.

## Web-framework dependency closure (`node_modules`, via `npm ci`)

The pug/express closure is NOT vendored. `prebuild.sh` runs `npm ci --omit=optional` from the
committed `assets/package.json` + `assets/package-lock.json` into a scratch dir and copies the
resulting `node_modules` into `/root/nweb`. The closure is pure JS (no native `.node` addons),
hence architecture-independent and valid for all four target arches. Direct dependencies:

| package | version | used by |
|:--|:--|:--|
| pug     | 3.0.3   | PugCarpet.js (template engine) |
| express | 4.21.2  | ExpressCarpet.js (web framework over IPv4 loopback) |

The full transitive closure (107 packages) is locked by `assets/package-lock.json`
(`lockfileVersion: 3`). `--omit=optional` drops optional native deps (none are required by the
carpets). To regenerate the lockfile after a version bump: `npm install` in a dir containing only
`assets/package.json`.

## Kotlin/JS module (`kotlin-app.js`, generated from `assets/kotlin-app.kt`)

`kotlin-app.js` is NOT committed. It is regenerated in `prebuild.sh` from the committed Kotlin
source `assets/kotlin-app.kt` by the **Kotlin 2.0.21** JS (IR) compiler, and staged next to the
KotlinJsCarpet under `/root/nweb/carpets/`. The emitted commonjs module is pure JS
(architecture-independent). `KotlinJsCarpet.js` runs it and asserts its stdout is byte-identical
to the golden `assets/kotlin-REF.out` (a small, hand-authored 6-line text fixture — it stays
committed) with thorough per-line / per-token checks.

### Kotlin compiler (fetched by sha256, cache-or-fetch)

| asset | url | sha256 |
|:--|:--|:--|
| kotlin-compiler-2.0.21.zip | https://github.com/JetBrains/kotlin/releases/download/v2.0.21/kotlin-compiler-2.0.21.zip | `0352c0a45bd22f80f6b26e485cd04da8047baa5de54865281fb9f89a4a7bcf2a` |

`prebuild.sh` uses a host `kotlinc-js` if one is on `PATH`; otherwise it fetches the pinned
compiler into the cache (default `<repo>/download/kotlinjs`, overridable via `KOTLINJS_CACHE` or
`$NODE_DL_ROOT/kotlinjs`) and runs it with the host JVM. The Kotlin JS compiler needs a JVM
(`java` on `PATH` or `$JAVA_HOME`).

### Regeneration recipe (exact commands `prebuild.sh` runs)

`STDLIB=<kotlinc>/lib/kotlin-stdlib-js.klib`. **`-ir-output-dir` MUST be an absolute path** (a
relative dir makes `kotlinc-js` silently emit no file), and **`-language-version 1.9` is
required** (the K2 default silently produces no JS from an `-Xinclude`d klib in 2.0.21). Two-step
IR flow (source → klib → commonjs js):

```
kotlinc-js -libraries "$STDLIB" -Xir-produce-klib-file \
  -ir-output-dir "$ABS/klib" -ir-output-name kotlin-app \
  -output "$ABS/klib/kotlin-app.klib" assets/kotlin-app.kt

kotlinc-js -Xir-produce-js -language-version 1.9 \
  -Xinclude="$ABS/klib/kotlin-app.klib" -libraries "$STDLIB" \
  -ir-output-dir "$ABS/js" -ir-output-name kotlin-app \
  -module-kind commonjs -main call
# -> $ABS/js/kotlin-app.js
```

The build is deterministic: with the pinned compiler + committed source it emits a whole-program
commonjs module that inlines the Kotlin stdlib (no DCE), **653669 bytes**, sha256
`a5a327d40870078089f405fd969936df0ee5676eb66ec96042f7162b404f8966`, whose stdout is byte-identical
to `assets/kotlin-REF.out`. `KotlinJsCarpet.js` asserts the file size in `[500000, 800000)` (a
tight range rather than one exact byte count, robust to a future Kotlin patch bump) plus the IR
polyfills hallmark, and — decisively — byte-exact stdout vs the golden.

### Optional pre-generated cache

If `$KOTLINJS_CACHE/kotlin-app.js` (default `<repo>/download/kotlinjs/kotlin-app.js`) exists and
matches the sha256 above, `prebuild.sh` uses it directly and skips the compiler download (fast
path for environments without a JVM / network — analogous to the offline apk cache). This cache
is NEVER committed; the source-build path above is the reproducible primary.

### Kotlin features exercised by `kotlin-app.kt` (one golden line each)

| line | feature | golden output |
|:--|:--|:--|
| 1 | data class + `componentN`/`copy` | `points=(21,12);(23,14);(25,16)` |
| 2 | sealed class + exhaustive `when` | `areas=12,12,3 total=27` |
| 3 | higher-order fns / lambdas / map/filter/sum | `evens^2 sum=220` |
| 4 | generics + extension functions | `second-of=b` |
| 5 | recursion | `fib(15)=610` |
| 6 | null-safety (`?.` `?:` `!!`) | `nullsafe=YES` |
