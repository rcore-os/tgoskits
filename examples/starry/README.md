# Starry Examples

`examples/starry/` contains runnable StarryOS scenarios. Most direct child
directories are board cases selected by `cargo starry example board -t <case>`;
some x86_64 QEMU demos provide their own `cargo xtask starry qemu` commands.

Cases are intentionally separate from `test-suit/starryos`: examples are
operator-facing workflows, while the test suit remains CI-oriented coverage.

## Case Layout

```text
examples/starry/<case>/
  init.sh
  build-<target>.toml
  board-<board>.toml
  <optional user projects>
```

- `init.sh` is read by `cargo starry example board` and sent to the Starry shell
  as the startup command.
- `build-<target>.toml` is the StarryOS build config. It must either include a
  top-level `target = "..."` or encode the target in the filename.
- `board-<board>.toml` is the ostool board run config. It supplies the board
  type, shell prefix, success/failure regexes, timeout, and optional server
  defaults.
- User programs under the case are examples only. The board rootfs must already
  contain the program and its shared libraries unless the case says otherwise.

Example:

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

## Orange Pi 5 Plus UVC

The `orangepi-5-plus-uvc` case needs `/usr/bin/uvc-fps` to be installed in the
board rootfs before StarryOS is booted. The usual preparation flow is:

1. reserve the board with `cargo board connect --board-type OrangePi-5-Plus`
   and leave that serial session open;
2. boot into the board Linux shell and read the board IP from the login banner
   or `ip -br addr`;
3. use SSH from the host to copy `examples/starry/orangepi-5-plus-uvc/uvc-fps/`
   into the board Linux system;
4. build and install `uvc-fps` on the board Linux rootfs;
5. close the `cargo board connect` session, then boot StarryOS with:

```bash
cargo starry example board -t orangepi-5-plus-uvc
```

See `orangepi-5-plus-uvc/README.md` for the complete copy, build, install, and
test commands.
