# consul-etcd binary provenance

`prebuild.sh` provisions all binaries by pinned identity into a portable cache
(`CONSUL_ETCD_DL_ROOT`, default under the app's staging root). No binaries are committed to
the repository. A clean checkout reproduces every cell from its official source.

## Consul 1.22.7

- Upstream: `https://github.com/hashicorp/consul`, tag `v1.22.7`
  (commit `c18bcb9db1fd73307ee8bf64a9bc17610d5427d5`, `c18bcb9d`).
- CGO-disabled fully static Go ELF.

| arch | source | identity |
|------|--------|----------|
| x86_64 | official release `consul_1.22.7_linux_amd64.zip` | sha256 `fe25cecd8dd3552a8e5b0941cde1d79bb6004eac384aa45679dd1398f947201d` |
| aarch64 | official release `consul_1.22.7_linux_arm64.zip` | sha256 `db54c5fb7c5ceaef97a38ca45dcc0f649ff592a48c73ab320e2d535c78e136cc` |
| riscv64 | cross-compiled from source tag `v1.22.7` | pinned source tag (output binary sha tracks the Go toolchain) |
| loongarch64 | cross-compiled from source tag `v1.22.7` | pinned source tag (output binary sha tracks the Go toolchain) |

Release download base: `https://releases.hashicorp.com/consul/1.22.7/`.

### Why riscv64 / loongarch64 are cross-compiled

HashiCorp publishes consul only for amd64 + arm64. Two indirect dependencies lack the arch
files, which is the real reason no release exists; `prebuild.sh` patches them in before a
`CGO_ENABLED=0 GOARCH=<goarch> go build`:

1. `github.com/boltdb/bolt@v1.3.1` (via `raft-boltdb/v2`) has no `bolt_riscv64.go` /
   `bolt_loong64.go` (missing `maxMapSize` / `maxAllocSize` / `brokenUnaligned`). The two
   added files use the same constants as `bolt_arm64.go` (all LP64, unaligned OK). boltdb
   v1.3.1 predates Go modules, so the local replace copy also gets a `go.mod`
   (`module github.com/boltdb/bolt` + `go 1.12`).
2. `github.com/shirou/gopsutil/v3@v3.22.9` has no `host_linux_loong64.go` (missing the
   `utmp` layout). loong64 and riscv64 are both LP64 little-endian with the same `utmp`
   layout, so `host_linux_riscv64.go` is copied to `host_linux_loong64.go` (the filename
   suffix is the build constraint; contents unchanged). riscv64 needs no gopsutil patch.

The linker stamps the version identically to the official release
(`-X .../version.GitVersion=1.22.7 -X .../version.GitCommit=c18bcb9d -X
.../version.GitDescribe=v1.22.7`), so all four arches report `Consul v1.22.7`.

## etcd 3.6.11

- Upstream: `https://github.com/etcd-io/etcd`, tag `v3.6.11`. v3.6.11 publishes official
  release tarballs for all four arches (amd64 / arm64 / riscv64 / loong64), so no
  cross-compile is needed. Each tarball bundles `etcd`, `etcdctl`, `etcdutl` (CGO-disabled
  static Go ELF).
- Release download base:
  `https://github.com/etcd-io/etcd/releases/download/v3.6.11/etcd-v3.6.11-linux-<goarch>.tar.gz`.

| arch | tarball | sha256 |
|------|---------|--------|
| x86_64 | `etcd-v3.6.11-linux-amd64.tar.gz` | `8756f7a4eaf921668a83de0bf13c0f65cae9186a165696e3ae8396afe6f557ed` |
| aarch64 | `etcd-v3.6.11-linux-arm64.tar.gz` | `5302f1a6157c34eb0568c75fba9d06da98353576df04399f08645bef634acd2d` |
| riscv64 | `etcd-v3.6.11-linux-riscv64.tar.gz` | `78ab006f4045c98a91cc8f435f80f7c4893f91b784d2be7adf9b623ac6e5b721` |
| loongarch64 | `etcd-v3.6.11-linux-loong64.tar.gz` | `cf2d3f51b63f1884805163a1d11dd3f6179fb10d39d447d8a5ce5dcc5a6f2a70` |

### `ETCD_UNSUPPORTED_ARCH` (riscv64 / loongarch64)

etcd has a tier-1 architecture gate: only amd64 / arm64 / ppc64le / s390x are "supported";
on other arches it refuses to start with

```
Refusing to run etcd on unsupported architecture since ETCD_UNSUPPORTED_ARCH is not set
```

The binary itself is fully functional - this is an etcd "you're on your own" prompt, not a
kernel limitation. The gate exports `ETCD_UNSUPPORTED_ARCH=riscv64` / `loong64` before
launching etcd on those arches; amd64/arm64 need no variable.

## GOARCH mapping

`x86_64 -> amd64`, `aarch64 -> arm64`, `riscv64 -> riscv64`, `loongarch64 -> loong64`.
