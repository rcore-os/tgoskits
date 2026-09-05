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

## Carpet contents (57 assertions, gate `CONSULETCD_OK = PASS/TOTAL`, all must pass)

Two complementary layers:

- **CLI surface** (help tree + flag matrix) - every subcommand's `--help` is walked across
  the full command tree of consul (35 top-level commands), etcdctl (50 subcommands) and
  etcdutl (8 subcommands), each ground-truthed against the official v1.22.7 / v3.6.11
  x86_64 releases; then the core commands' real flag behavior is driven against the live
  daemons. This surface is binary-carried, so it is identical across the four arches (the
  cross-compiled rv64/la64 binaries are the same version).
- **Runtime** - each daemon exercised on its own through its real client path, then both
  driven concurrently in the integration workflow.

### Consul CLI surface (help tree, no daemon)

| # | assertion |
|---|-----------|
| 1 | `consul --help` lists the command set (`agent` / `kv` / `catalog` / ...) |
| 2 | full top-level `--help` tree - all 34 subcommands exit 0 + print their usage token |
| 3 | `consul kv` subtree (`delete` / `export` / `get` / `import` / `put`) |
| 4 | `consul catalog` + `consul snapshot` subtrees (datacenters/nodes/services + decode/inspect/restore/save) |
| 5 | `consul acl` / `connect` / `intention` / `operator` subtrees (auth + service-mesh + operator, 21 subcommands) |
| 6 | `consul config` / `services` / `peering` / `resource` subtrees (12 subcommands) |

### Consul runtime (single-node `consul agent -dev` on loopback)

| # | assertion |
|---|-----------|
| 7 | `consul version` red-line = exact `Consul v1.22.7` |
| 8 | dev agent reaches `Consul agent running!` (raft + serf + gRPC/HTTP/DNS listeners) |
| 9 | `consul members` shows `starrynode` `alive` (client RPC round-trip) |
| 10 | `consul kv put` / `kv get` byte-exact round-trip |
| 11 | `consul kv get -recurse` returns all three key/value pairs |
| 12 | `consul kv get -keys` lists the keys |
| 13 | `consul kv put -flags=42` + `kv get -detailed` surfaces `Flags 42` |
| 14 | `consul kv put -cas -modify-index=0` is create-only (first write ok, second fails "already exists") |
| 15 | `consul kv get -keys -separator=/` collapses a nested prefix into one folder entry |
| 16 | `consul kv delete` -> `kv get` exits non-zero with `No key exists` |
| 17 | `consul services register` + `consul catalog services` lists `web` |
| 18 | `consul catalog services -tags` appends the service's tags |
| 19 | `consul catalog datacenters` lists `dc1` |
| 20 | `consul catalog nodes` lists `starrynode` |
| 21 | `consul operator raft list-peers` shows `starrynode` as `leader` + voter |
| 22 | health check: the `web` TCP check reaches `passing` (`consul watch -type=checks -state=passing`) |
| 23 | `consul snapshot save` writes a verified snapshot file |
| 24 | `consul snapshot inspect` reads the snapshot metadata (ID / Index / Term) |
| 25 | `consul snapshot restore` restores it into the running server |

### etcd CLI surface (help tree, no daemon)

| # | assertion |
|---|-----------|
| 26 | `etcd --help` (server flags, `--data-dir` present) |
| 27 | `etcdctl --help` lists the grouped command set (`get` / `put` / `lease grant` / ...) |
| 28 | full etcdctl `--help` tree - all 50 subcommands exit 0 + print their usage token |
| 29 | `etcdctl auth` / `user` / `role` surface (multi-tenant auth CLI, 16 subcommands) |
| 30 | `etcdutl --help` lists its command set |
| 31 | full etcdutl `--help` tree - all 8 subcommands (incl `snapshot restore` / `snapshot status`) |

### etcd runtime (single-node server on loopback)

| # | assertion |
|---|-----------|
| 32 | `etcd --version` red-line = `etcd Version: 3.6.11` |
| 33 | `etcdctl version` red-line = `etcdctl version: 3.6.11` |
| 34 | server reaches `endpoint health` = `is healthy` (Raft + bbolt up) |
| 35 | `etcdctl put` / `get` byte-exact round-trip |
| 36 | `etcdctl put --prev-kv` returns the prior key/value it replaced |
| 37 | `etcdctl del` -> `get` returns empty |
| 38 | `etcdctl get --prefix` matrix: `--keys-only` / `--limit` / `--sort-by=KEY --order=DESCEND` / `-w json` count |
| 39 | `etcdctl del --prefix` returns the count of keys removed |
| 40 | `etcdctl watch` (background) receives the `PUT` event triggered by a later `put` |
| 41 | `etcdctl txn` `value()` guard takes the success branch and applies its writes |
| 42 | `etcdctl txn` `mod()` revision guard takes the success branch (distinct guard operator) |
| 43 | `etcdctl lease grant` + `put --lease` + `lease keep-alive --once` + `lease timetolive` |
| 44 | `etcdctl lease list` reports the live lease (count line + hex id) |
| 45 | lease TTL expiry: a key on a short lease with no keep-alive is auto-removed |
| 46 | `etcdctl member list` shows member `s1` `started` |
| 47 | `etcdctl member list -w json` exposes the machine-readable members array |
| 48 | `etcdctl endpoint status -w table` renders the status table header |
| 49 | `etcdctl endpoint hashkv` returns the KV-history hash |
| 50 | `etcdctl alarm list` on a healthy store is empty + exits 0 |
| 51 | `etcdctl compaction <rev>` compacts history to a read-back revision |
| 52 | `etcdctl snapshot save` writes a snapshot file, verified by `etcdutl snapshot status` |
| 53 | `etcdutl snapshot status -w table` renders the offline snapshot metadata table |

### Integration (consul + etcd running concurrently - service discovery + config center)

| # | assertion |
|---|-----------|
| 54 | consul agent and etcd server serve CONCURRENTLY on loopback (distinct port sets, two heavy Go daemons coexisting) |
| 55 | register a service in consul, then discover it via `consul catalog services` |
| 56 | store the service's config in etcd (`config/<svc>/dsn`, `config/<svc>/replicas`) and read it back byte-exact |
| 57 | end-to-end: the name discovered from consul keys the etcd config lookup, and the DSN round-trips - proving the two systems compose into a discover-then-configure flow |

The `--help` trees are all-or-nothing aggregate assertions: a single regressed subcommand
fails the whole tree, so there is no silent partial coverage.

Multi-node raft/gossip clustering is intentionally out of scope: a StarryOS single VM has
no second host, and real clustering needs multiple VMs or network namespaces.

## Judgement authority

`cargo xtask starry app qemu -t consul-etcd --arch <arch>` -> `rc=0` + log
`SUCCESS PATTERN MATCHED` + `success_regex` on `TEST PASSED` + anchored
`CONSULETCD_OK=57/57`.

## Architecture coverage and kernel dependencies

| arch | consul source | etcd source | kernel dependency |
|------|---------------|-------------|-------------------|
| x86_64 | official amd64 release | official amd64 release | mmap-EOF populate fix (etcd bbolt) |
| aarch64 | official arm64 release | official arm64 release | `-cpu cortex-a72` |
| riscv64 | cross-compiled from source | cross-compiled from source | getrlimit->prlimit64 routing (consul); mmap-EOF fix; `ETCD_UNSUPPORTED_ARCH=riscv64` |
| loongarch64 | cross-compiled from source | cross-compiled from source | dynamic platform (EFI console, `uefi=true`); `ETCD_UNSUPPORTED_ARCH=loong64` |

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
- **`IP_PKTINFO` / `IPV6_RECVPKTINFO` setsockopt (all arches)**: consul's serf/memberlist
  gossip (and the miekg/dns UDP server when enabled) request ancillary destination-address
  delivery; the setsockopt whitelist (`syscall/net/opt.rs`) rejected it with `ENOPROTOOPT`.
  Fixed: accept these options like Linux (functional on a single loopback address without cmsg
  delivery). The carpet runs the dev agent with `-dns-port=-1` - it exercises serf/raft/HTTP/KV,
  never DNS - because binding the DNS listener has a fixed internal startup deadline that a slow
  emulated-arch TCG run can miss ("timeout starting DNS servers"), aborting the agent.

## Binary provenance (see `programs/SOURCES.md`)

`prebuild.sh` provisions every binary by pinned identity into a portable cache
(`CONSUL_ETCD_DL_ROOT`); nothing is committed:

- consul x86_64/aarch64: official HashiCorp release zip, sha256-pinned download.
- consul riscv64/loong64: HashiCorp ships no release (two indirect deps lack the arch
  files); cross-compiled in-prebuild from the pinned source tag `v1.22.7`
  (`CGO_ENABLED=0`, `GOARCH=<goarch>`) with `boltdb` + `gopsutil` arch files patched in.
- etcd x86_64/aarch64: official etcd-io release tarball, sha256-pinned download.
- etcd riscv64/loong64: etcd-io ships no linux binary for these arches (releases cover
  amd64 / arm64 / ppc64le / s390x only); `etcd` / `etcdctl` / `etcdutl` are cross-compiled
  in-prebuild from the pinned source tag `v3.6.11` (`CGO_ENABLED=0`, `GOARCH=<goarch>`).

## Prerequisites

- **All arches**: QEMU 10, musl cross toolchains, and Rust/cargo toolchain (set up by
  sourcing `.starry-env.sh` in the repository root).
- **riscv64 and loongarch64 only** (both consul and etcd are source-built - neither ships
  an official release for these arches): `go` >= 1.25 (etcd's minimum toolchain; consul
  itself only needs >= 1.22, so 1.25 satisfies both) and `git` must be in `$PATH` when
  `prebuild.sh` runs. If they are absent and no pre-populated `$CONSUL_ETCD_DL_ROOT` cache
  exists, `prebuild.sh` exits with an error.

## Run

```bash
source .starry-env.sh                          # qemu-10 + musl crosses
for a in x86_64 aarch64 riscv64 loongarch64; do
  cargo xtask starry app qemu -t consul-etcd --arch "$a"
done
```
