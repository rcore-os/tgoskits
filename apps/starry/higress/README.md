# higress

> higress standalone gateway (Envoy data plane, static bootstrap) on StarryOS

higress is a cloud-native API gateway whose data plane is Envoy. In standalone
mode it runs the Envoy binary against a static bootstrap (no xDS control plane),
so a single process provides the gateway functionality. This app runs Envoy
1.38.3 directly on StarryOS as a dynamic ELF on all four architectures and
asserts the full documented gateway data path end to end (65 assertions).

## 架构矩阵

| 架构 | Envoy 1.38.3 | 状态 |
|------|--------------|------|
| x86_64 | 官方 release `envoy-1.38.3-linux-x86_64` (glibc) | A 档，实跑 65/65 |
| aarch64 | 官方 release `envoy-1.38.3-linux-aarch_64` (glibc) | A 档，实跑 65/65 |
| riscv64 | 源码建 (clang-18 + musl cross, lld) | A 档，实跑 65/65 (musl-dynamic ELF，同 x86/aa 走同一 `run-higress.sh`) |
| loongarch64 | 源码建 (clang-18 + musl cross, lld) | A 档，实跑 65/65 (musl-dynamic ELF，同 x86/aa 走同一 `run-higress.sh`) |

Envoy ships prebuilt binaries only for glibc x86_64 + aarch64
(`github.com/envoyproxy/envoy/releases`), so those two run the stock release ELF.
Upstream has no riscv64 / loongarch64 build, so for those two `prebuild.sh`
source-builds the same Envoy 1.38.3 from the pinned source tag with clang-18
against a musl cross sysroot, linked with lld - see
`assets/build-envoy-rvloong.sh`. The build keeps only the data-plane extensions
the carpet exercises (HTTP connection manager, router, local rate limit, static
clusters, BoringSSL TLS) and drops the families that need a per-arch runtime with
no rv64/loong64 port (V8/wasm, LuaJIT, the Go filter, the Rust dynamic-modules /
Hickory resolver, the datadog / opentelemetry tracers). The output is a
musl-dynamic ELF; the build is cached under `HIGRESS_CACHE` so it runs once.

x86_64 / aarch64 downloads are pinned by version + SHA256 in `prebuild.sh`:

| 架构 | SHA256 |
|------|--------|
| x86_64 | `affffb8d08a14fdc375b1f7dd8d0f3004eacdf51ce07f5636d7e168a01c6b373` |
| aarch64 | `eff9766ce1a7af71c38a6d4587367621753049ae3df1bde5b6b9e695752f3167` |

riscv64 / loongarch64 are reproducible from the pinned Envoy 1.38.3 source tag
(there is no upstream binary to SHA-pin); `assets/build-envoy-rvloong.sh` fetches
and patches that tag deterministically.

## 运行方式

StarryOS loads the dynamic Envoy ELF through PT_INTERP, the same mechanism proven
by `apps/starry/glibc-dynamic-smoke` (glibc) and the Alpine musl base (musl).
`prebuild.sh` reads the interpreter and NEEDED sonames straight from the Envoy
binary and stages them into the overlay - the glibc loader + libs from the arch's
`<arch>-linux-gnu` cross sysroot for x86_64/aarch64, or the C++ runtime from the
musl cross sysroot for riscv64/loongarch64 (the musl libc + loader are the Alpine
base itself):

| 架构 | INTERP | 额外 staged 运行库 |
|------|--------|--------------------|
| x86_64 | `/lib64/ld-linux-x86-64.so.2` | glibc loader + libc/libm/librt/libdl/libpthread |
| aarch64 | `/lib/ld-linux-aarch64.so.1` | glibc loader + libc/libm/librt/libdl/libpthread |
| riscv64 | `/lib/ld-musl-riscv64.so.1` (Alpine base) | libstdc++.so.6 + libgcc_s.so.1 |
| loongarch64 | `/lib/ld-musl-loongarch64.so.1` (Alpine base) | libstdc++.so.6 + libgcc_s.so.1 |

The official x86_64/aarch64 Envoy statically links BoringSSL and libstdc++, so on
glibc no extra C++ runtime is staged (max symbol requirement is GLIBC_2.30). The
riscv64/loongarch64 source build links libstdc++ dynamically (GCC's static
libstdc++.a carries .eh_frame relocations into discarded COMDAT sections that lld
rejects), so `prebuild.sh` stages `libstdc++.so.6` + `libgcc_s.so.1` from the musl
cross sysroot; BoringSSL is still statically linked into the Envoy binary.

```bash
cargo xtask starry app qemu -t higress --arch x86_64
cargo xtask starry app qemu -t higress --arch aarch64
cargo xtask starry app qemu -t higress --arch riscv64
cargo xtask starry app qemu -t higress --arch loongarch64
```

### 自包含的上游 / 客户端（不依赖 guest 网络）

The carpet never touches in-guest networking (`apk`) at runtime. The Alpine base
busybox is built without the `httpd` applet, so `prebuild.sh` provisions the test
upstreams entirely at build time:

- **`echod`** - a tiny HTTP echo backend (`backend/echod.c`) cross-compiled to a
  static musl binary. It echoes the received method / request URI / selected
  request headers and emits a fixed `X-Backend-Secret` response header, which is
  what lets the carpet assert routing, rewriting and header mutation.
- **`openssl`** - the CLI is pulled from the matching Alpine branch (the base
  rootfs already carries `libssl.so.3` / `libcrypto.so.3`) and drives the
  downstream TLS client (`s_client`) and the upstream TLS backend (`s_server`).

The plaintext client is busybox `wget`; custom-method / custom-header / TLS
requests go through `openssl s_client` against the downstream-TLS listener.

## 内核依赖: SO_REUSEPORT

Envoy defaults `enable_reuse_port=true` on Linux, so it calls
`setsockopt(SO_REUSEPORT)` on every listener socket even with `--concurrency 1`.
StarryOS previously returned `ENOPROTOOPT` for that option, which would fail
listener creation. The kernel now implements SO_REUSEPORT (accept + store the
flag, and let a reuseport group share a bound port); see
`test-suit/starryos/qemu-smp1/system/syscall-test-so-reuseport` for the
regression test. The baseline `conf/bootstrap.yaml` pins
`enable_reuse_port:false` so the gateway features can be exercised independently;
the carpet then restarts Envoy against the generated `bootstrap-reuseport.yaml`
(`enable_reuse_port:true`) to prove the option is accepted.

## 覆盖的网关能力

`programs/run-higress.sh` starts the echod backends (200 / 503 / slow), an
openssl `s_server` TLS upstream, and Envoy against the static bootstrap, then
asserts the documented data plane (gate `HIGRESS_OK=65/65`):

- CLI 面: `envoy --version`（版本红线 1.38.3）、`--mode validate`（好配置过 / 坏配置拒）、`--help`
- admin 面: `/ready`、`/stats`、`/server_info`、`/clusters`、`/listeners`、`/stats?filter=`、`/stats?format=prometheus`、`/config_dump`、`/certs`、未知路径 404
- 路由匹配: prefix / 精确 path / safe_regex / query-parameter / header 五种 match
- 负载均衡: 加权 (2:1)、round_robin (多 endpoint)、least_request、random
- 请求头改写: 注入 (`x-higress-added`) 与删除 (`x-strip-me`)
- 响应头改写: 注入 (`x-higress-gateway`) 与删除 (`x-backend-secret`)
- 路径改写: prefix_rewrite、regex_rewrite（带捕获组）、host_rewrite_literal
- 动作: redirect (301 + Location)、direct_response (200 body / 404 body)
- 每路由本地限流 (`local_ratelimit` token bucket -> 429)
- 上游重试 (`retry_on: 5xx`, `num_retries: 2`，命中 `upstream_rq_retry`)
- 异常: dead upstream (503)、路由超时 (504)、上游 TLS 证书校验失败 (503)
- 下游 TLS 终止 (`:10443`，DownstreamTlsContext，TLSv1.2+，证书 CN=localhost)
- 上游 TLS (`backend_tls` cluster，UpstreamTlsContext) + 下游/上游 TLS 串联一跳
- SO_REUSEPORT listener（`enable_reuse_port:true` 起得来并服务）

判定门控：`HIGRESS_OK=<pass>/<total>`，且仅当全部 65 条断言通过时尾部 `printf`
输出 `TEST PASSED`（qemu `success_regex = ["^TEST PASSED$"]`）。

## Host 验证

`host-validate/validate.sh` 用真实 Envoy x86_64 + `echod` 后端 + openssl + curl
复现同一 `conf/bootstrap.yaml` 的全部断言（host-only 开发者工具，非 guest 镜像的一部分）：

```bash
STARRY_ARCH=x86_64 STARRY_OVERLAY_DIR=/tmp/hg-overlay bash prebuild.sh   # stage envoy
bash host-validate/validate.sh
```

`envoy --mode validate -c conf/bootstrap.yaml` 通过 schema 校验；上述 host 断言全绿（`HIGRESS_HOST_OK=65/65`）。
