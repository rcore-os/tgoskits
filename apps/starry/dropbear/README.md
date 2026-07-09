# dropbear - StarryOS SSH suite carpet

A carpet-level test of the [dropbear](https://matt.ucc.asn.au/dropbear/dropbear.html) SSH
suite on StarryOS. Every component - the server (`dropbear`), client (`dbclient`), key
generator (`dropbearkey`), key-format converter (`dropbearconvert`), scp and the
`ssh`->`dbclient` symlink - is driven single-node over IPv4 loopback: keys of every type are
generated, the server is brought up under several policies, `dbclient` runs real
key-authenticated sessions, files are copied with scp, keys are round-tripped through the
OpenSSH format, and TCP port forwarding is tunnelled end to end. The gate provisions the
suite on-target with `apk add` (branch-matched to the rootfs, current version - no pinned
URL) and skips it when the binaries are already staged.

## Run

```
cargo xtask starry app qemu -t dropbear --arch x86_64
cargo xtask starry app qemu -t dropbear --arch aarch64
cargo xtask starry app qemu -t dropbear --arch riscv64
cargo xtask starry app qemu -t dropbear --arch loongarch64
```

The gate (`programs/run-dropbear.sh`) is the entire `shell_init_cmd`. It prints `TEST PASSED`
only when every assertion passes AND the count equals the pinned `EXPECTED` total, so a
skipped or dropped assertion fails the gate. The last line is `DROPBEAR_OK=50/50`.

## Coverage (50 assertions)

| section | assertions | what is exercised |
|---------|-----------|-------------------|
| A. binary self-certification | A1-A9 | `dropbear -V`/`-h` (server surface incl. `-j`/`-k`/`-a`/`-c`), `dropbearkey` usage, `dbclient -V`/usage, `dropbearconvert` usage, `scp` usage, `ssh`->`dbclient` symlink, and the client forwarding surface (`-L`/`-R`/`-B` present, no dynamic-SOCKS `-D`) |
| B. dropbearkey | B1-B12 | rsa 2048/3072, ecdsa 256/384/521, ed25519 generation; `-y` public-key + SHA256 fingerprint read-back for each type; `-C` comment; invalid-type rejection; `-t dss` rejection (DSS is not built into this dropbear) |
| C. server + dbclient | C1-C14 | server up and accepting loopback SSH; key-authenticated sessions per key type (ed25519/rsa/ecdsa); remote command with args; `-p` port; `-c` cipher and `-m` MAC selection; `-T` no-pty; wrong-key rejection; `-P` pidfile; `-w` root-lockout rejection; `-p address:port` bind; `-c` forced command override |
| D. dropbearconvert | D1-D6 | dropbear->openssh PEM emit; openssh->dropbear round-trip; ecdsa public half preserved and the converted key authenticates a real session; rsa and ed25519 round-trips preserve the public half |
| E. integration | E1-E3 | end-to-end fresh keygen -> server -> authenticated session -> remote exec -> scp file transfer over the dropbear link |
| F. port forwarding | F1-F6 | `-L` local forward carries a real service banner through the tunnel; `-R` remote forward; `-B` netcat-alike forward; `-g` gateway ports; server `-j` blocks local forwarding; server `-k` blocks remote forwarding |

Key generation, all authentication, the scp transfer and every forward run against the real
dropbear processes; the negative paths (wrong key, `-w` root lockout, `-j`/`-k` forwarding
lockouts) are asserted to be rejected. The forwards tunnel the live SSH identification banner
of a second dropbear instance, so a passing forward proves real service bytes crossed the
link.

## Algorithm / feature notes

Alpine's dropbear `2025.88` is built without DSS/DSA: `dropbearkey` offers only
`rsa`/`ecdsa`/`ed25519`, there is no `ssh-dss` host-key algorithm in the server, and `-t dss`
is rejected - so there is no DSS host key to generate (B12 asserts the rejection). dropbear's
`dbclient` also has no dynamic-SOCKS `-D` option (it was never implemented); its forwarding
surface is `-L` (local), `-R` (remote), `-B` (netcat-alike) and `-g`, all exercised in
section F (A9 asserts `-D` is absent).

## Provisioning

On-target `apk add dropbear dropbear-dbclient dropbear-convert dropbear-scp dropbear-ssh`
against the branch that matches `/etc/alpine-release`, so apk resolves the current version
and its dependency closure (musl / libutmps / libz) with no committed binary and no pinned,
drifting URL. See `programs/SOURCES.md`. The gate skips apk when the suite is already present
(host chroot pre-flight), so the identical script validates on the host before QEMU.

## Layout

- `prebuild.sh` - stages the suite + gate and grows the per-app rootfs for the keys/logs.
- `programs/run-dropbear.sh` - the on-target carpet gate.
- `programs/SOURCES.md` - package provenance.
- `build-<target>.toml` x4 / `qemu-<arch>.toml` x4 - per-arch build features and QEMU boot
  (loongarch64 is dynamic-platform, mirroring aarch64/riscv64).
