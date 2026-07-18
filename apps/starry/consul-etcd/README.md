# consul-etcd - Consul + etcd distributed-KV carpet

Two production distributed systems run single-node on StarryOS across the four
architectures (x86_64 / aarch64 / riscv64 / loongarch64) under qemu-10, single core:

- **Consul 1.22.7** - HashiCorp service discovery / KV / health checks / serf gossip
  over an embedded raft state store.
- **etcd 3.6.11** - Raft consensus + bbolt MVCC KV store over gRPC.

Both are fully static, CGO-disabled Go ELF binaries (no libc, no interpreter): the Go
runtime gets entropy via `getrandom(2)` and parks goroutines via `futex`; loopback
`AF_INET` TCP/UDP carries serf/raft/HTTP (consul) and client/peer RPC (etcd). The gate
(`programs/run-consul-etcd.sh`) drives both through their real client paths and only
prints `TEST PASSED` when every assertion of both carpets passed.

## Carpet contents (27 assertions, gate `CONSULETCD_OK = PASS/TOTAL`, all must pass)

The isolated carpets (each daemon exercised on its own, assertions 1-23) are the prerequisite;
the integration section (assertions 24-27) is layered on top as a combination test.

### Consul (single-node `consul agent -dev` on loopback)

| # | assertion |
|---|-----------|
| 1 | `consul version` red-line = exact `Consul v1.22.7` |
| 2 | dev agent reaches `Consul agent running!` (raft + serf + gRPC/HTTP/DNS listeners) |
| 3 | `consul members` shows `starrynode` `alive` (client RPC round-trip) |
| 4 | `consul kv put` / `kv get` byte-exact round-trip |
| 5 | `consul kv get -recurse` returns all three key/value pairs |
| 6 | `consul kv get -keys` lists the keys |
| 7 | `consul kv delete` -> `kv get` exits non-zero with `No key exists` |
| 8 | `consul services register` + `consul catalog services` lists `web` |
| 9 | `consul catalog nodes` lists `starrynode` |
| 10 | health check: the `web` TCP check reaches `passing` (`consul watch -type=checks -state=passing`) |
| 11 | `consul snapshot save` writes a verified snapshot file |
| 12 | `consul snapshot restore` restores it into the running server |

### etcd (single-node server on loopback)

| # | assertion |
|---|-----------|
| 13 | `etcd --version` red-line = `etcd Version: 3.6.11` |
| 14 | `etcdctl version` red-line = `etcdctl version: 3.6.11` |
| 15 | server reaches `endpoint health` = `is healthy` (Raft + bbolt up) |
| 16 | `etcdctl put` / `get` byte-exact round-trip |
| 17 | `etcdctl del` -> `get` returns empty |
| 18 | `etcdctl watch` (background) receives the `PUT` event triggered by a later `put` |
| 19 | `etcdctl txn` guarded transaction takes the success branch and applies its writes |
| 20 | `etcdctl lease grant` + `put --lease` + `lease keep-alive --once` + `lease timetolive` |
| 21 | lease TTL expiry: a key on a short lease with no keep-alive is auto-removed |
| 22 | `etcdctl member list` shows member `s1` `started` |
| 23 | `etcdctl snapshot save` writes a snapshot file, verified by `etcdutl snapshot status` |

### Integration (consul + etcd running concurrently - service discovery + config center)

| # | assertion |
|---|-----------|
| 24 | consul agent and etcd server serve CONCURRENTLY on loopback (distinct port sets, two heavy Go daemons coexisting) |
| 25 | register a service in consul, then discover it via `consul catalog services` |
| 26 | store the service's config in etcd (`config/<svc>/dsn`, `config/<svc>/replicas`) and read it back byte-exact |
| 27 | end-to-end: the name discovered from consul keys the etcd config lookup, and the DSN round-trips - proving the two systems compose into a discover-then-configure flow |

Multi-node raft/gossip clustering is intentionally out of scope: a StarryOS single VM has
no second host, and real clustering needs multiple VMs or network namespaces.

## Judgement authority

`cargo xtask starry app qemu -t consul-etcd --arch <arch>` -> `rc=0` + log
`SUCCESS PATTERN MATCHED` + `success_regex` on `TEST PASSED` + anchored
`CONSULETCD_OK=27/27`.

## Architecture coverage and kernel dependencies

| arch | consul source | etcd source | kernel dependency |
|------|---------------|-------------|-------------------|
| x86_64 | official amd64 release | official amd64 release | mmap-EOF populate fix (etcd bbolt) |
| aarch64 | official arm64 release | official arm64 release | `-cpu cortex-a72` |
| riscv64 | cross-compiled from source | official riscv64 release | getrlimit->prlimit64 routing (consul); mmap-EOF fix; `ETCD_UNSUPPORTED_ARCH=riscv64` |
| loongarch64 | cross-compiled from source | official loong64 release | dynamic platform; `ETCD_UNSUPPORTED_ARCH=loong64` |

- **getrlimit routing**: consul's Go runtime calls legacy `getrlimit` directly on riscv64;
  StarryOS routes `getrlimit`/`setrlimit` to the existing `sys_prlimit64(pid=0)` on every
  arch (`struct rlimit` == two u64 == `rlimit64` on all LP64 arches), so consul no longer
  aborts with ENOSYS.
- **mmap-EOF populate fix**: etcd's bbolt mmaps its db with a ~10 GB `InitialMmapSize`; the
  `FileBackend::populate` path skips pages past EOF (SIGBUS semantics, Linux-aligned) instead
  of eagerly allocating frames for the whole sparse range, which otherwise OOMs. Merged on `dev`.
- **fdatasync on a read-only shared mmap (all arches)**: bbolt mmaps its db read-only and
  writes through `pwrite`, so on `fdatasync` the page-cache writeback finds the dirty page
  mapped read-only. `protect_dirty_page` (`mm/aspace/backend/file.rs`) treated a non-writable
  4K mmap page as an "unexpected page size" and returned false -> `ResourceBusy` -> etcd got a
  fatal `fdatasync EBUSY`. Fixed: a read-only shared page cannot be dirtied through the mapping,
  so nothing needs write-protecting - report success. Distinct from the mmap-EOF fix.
- **`IP_PKTINFO` / `IPV6_RECVPKTINFO` setsockopt (all arches)**: consul's UDP DNS server
  (miekg/dns) and serf/memberlist enable ancillary destination-address delivery; the setsockopt
  whitelist (`syscall/net/opt.rs`) rejected it with `ENOPROTOOPT`, killing the agent right after
  the DNS server started. Fixed: accept these options like Linux (functional on a single loopback
  address without cmsg delivery).

## Binary provenance (see `programs/SOURCES.md`)

`prebuild.sh` provisions every binary by pinned identity into a portable cache
(`CONSUL_ETCD_DL_ROOT`); nothing is committed:

- consul x86_64/aarch64: official HashiCorp release zip, sha256-pinned download.
- consul riscv64/loong64: HashiCorp ships no release (two indirect deps lack the arch
  files); cross-compiled in-prebuild from the pinned source tag `v1.22.7`
  (`CGO_ENABLED=0`, `GOARCH=<goarch>`) with `boltdb` + `gopsutil` arch files patched in.
- etcd all four arches: official etcd-io release tarball, sha256-pinned download
  (v3.6.11 ships amd64 / arm64 / riscv64 / loong64).

## Prerequisites

- **All arches**: QEMU 10, musl cross toolchains, and Rust/cargo toolchain (set up by
  sourcing `.starry-env.sh` in the repository root).
- **riscv64 and loongarch64 only** (consul source build, no official release):
  `go` >= 1.22 and `git` must be in `$PATH` when `prebuild.sh` runs. If they are absent
  and no pre-populated `$CONSUL_ETCD_DL_ROOT` cache exists, `prebuild.sh` exits with
  an error.

## Run

```bash
source .starry-env.sh                          # qemu-10 + musl crosses
for a in x86_64 aarch64 riscv64 loongarch64; do
  cargo xtask starry app qemu -t consul-etcd --arch "$a"
done
```
