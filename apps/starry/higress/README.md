# higress

> higress standalone gateway (Envoy data plane, static bootstrap) on StarryOS

higress is a cloud-native API gateway whose data plane is Envoy. In standalone
mode it runs the stock Envoy binary against a static bootstrap (no xDS control
plane), so a single process provides the gateway functionality. This app runs
the official Envoy release directly on StarryOS as a glibc dynamic ELF and
asserts the full gateway data path end to end.

## 架构矩阵

| 架构 | Envoy release | 状态 |
|------|---------------|------|
| x86_64 | `envoy-1.38.3-linux-x86_64` | A 档，实跑 |
| aarch64 | `envoy-1.38.3-linux-aarch_64` | A 档，实跑 |
| riscv64 | 无 | 上游 Envoy 无 riscv64 端口 |
| loongarch64 | 无 | 上游 Envoy 无 loongarch64 端口 |

Envoy ships prebuilt binaries only for glibc x86_64 + aarch64
(`github.com/envoyproxy/envoy/releases`). Upstream has no riscv64 / loongarch64
port and no musl build, so those two architectures are out of scope here - this
is an upstream ecosystem gap, not a StarryOS limitation. A source Bazel port to
those targets is a separate long-horizon effort.

Envoy binaries are pinned by version + SHA256 in `prebuild.sh`:

| 架构 | SHA256 |
|------|--------|
| x86_64 | `affffb8d08a14fdc375b1f7dd8d0f3004eacdf51ce07f5636d7e168a01c6b373` |
| aarch64 | `eff9766ce1a7af71c38a6d4587367621753049ae3df1bde5b6b9e695752f3167` |

## 运行方式

StarryOS loads the glibc dynamic Envoy ELF through PT_INTERP, the same mechanism
proven by `apps/starry/glibc-dynamic-smoke`. `prebuild.sh` reads the interpreter
and NEEDED sonames straight from the Envoy binary and stages them from the arch's
`<arch>-linux-gnu` cross sysroot into the overlay:

| 架构 | INTERP | NEEDED |
|------|--------|--------|
| x86_64 | `/lib64/ld-linux-x86-64.so.2` | libc/libm/librt/libdl/libpthread |
| aarch64 | `/lib/ld-linux-aarch64.so.1` | libc/libm/librt/libdl/libpthread |

Envoy 1.38.3 statically links BoringSSL and libstdc++, so TLS works without any
extra runtime library (max symbol requirement is GLIBC_2.30).

```bash
cargo xtask starry app qemu -t higress --arch x86_64
cargo xtask starry app qemu -t higress --arch aarch64
```

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

The carpet (`programs/run-higress.sh`) starts three busybox httpd CGI backends,
a 503 backend, and an openssl `s_server` TLS upstream, then asserts through Envoy:

- 前缀 / host 路由（`app.example.com` vhost，prefix 匹配）
- 加权负载均衡（backend_a:backend_b = 2:1）
- 请求头注入（`x-higress-added`）与响应头改写（`x-higress-gateway`）
- 路径改写（`/api` -> `prefix_rewrite /cgi-bin/echo`）
- 上游重试（`retry_on: 5xx`, `num_retries: 2`，命中 `upstream_rq_retry` 计数）
- 每路由本地限流（`local_ratelimit` token bucket -> 429）
- 下游 TLS 终止（`:10443`，DownstreamTlsContext）
- 上游 TLS（`backend_tls` cluster，UpstreamTlsContext）
- admin 端点（`:9901/ready`、`:9901/stats`）
- SO_REUSEPORT listener（`enable_reuse_port:true` 起得来并服务）

判定门控：`HIGRESS_OK=<pass>/<total>`，且仅当全部断言通过时尾部 `printf`
输出 `TEST PASSED`（qemu `success_regex = ["^TEST PASSED$"]`）。

## Host 验证

`host-validate/validate.sh` 用真实 Envoy x86_64 + python echo 后端 + curl 复现同一
`conf/bootstrap.yaml` 的全部断言（host-only，非 guest 镜像的一部分）：

```bash
STARRY_ARCH=x86_64 STARRY_OVERLAY_DIR=/tmp/hg-overlay bash prebuild.sh   # stage envoy
bash host-validate/validate.sh
```

`envoy --mode validate -c conf/bootstrap.yaml` 通过 schema 校验；上述 host 真跑
断言全绿（含加权 LB、限流 429、下游/上游 TLS、reuse_port）。
