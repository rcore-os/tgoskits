# Starry Examples

`examples/starry/` contains runnable StarryOS scenarios. Each direct child
directory is a case selected by `cargo starry example board -t <case>`.

Cases are intentionally separate from `test-suit/starryos`: examples are
operator-facing board workflows, while the test suit remains CI-oriented
coverage.

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
