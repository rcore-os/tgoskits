# Expected Backtrace Demo Markers

Use this file as a quick checklist while reviewing logs. The commands are listed
in [README.md](README.md).

## Demo 1: Raw Backtrace

Must contain:

```text
Running backtrace tests...
BACKTRACE_BEGIN
BT 0 ip=0x... fp=0x...
BT 1 ip=0x... fp=0x...
BACKTRACE_END
test pass
```

Must not require:

```text
=== host backtrace symbolize ===
```

`--no-symbolize` intentionally disables host symbolization.

## Demo 2: Auto Host Symbolize

Must contain:

```text
Running backtrace tests...

test pass


=== host backtrace symbolize ===
BACKTRACE_BLOCK
BT 0 ip=0x... fp=0x... [function name]
BT 1 ip=0x... fp=0x... [function name]
ok: backtrace
```

The exact function names can vary with optimization and toolchain details, but
the host-side block should not be empty.

## Demo 3: DWARF Auto Symbolize

Must contain:

```text
emitting raw backtrace report (normal fp chain)...

test pass

=== host backtrace symbolize ===
BACKTRACE_BLOCK 0 kind=raw
```

When DWARF/debug info is available, the symbolized block should include the
synthetic call chain from `test-suit/arceos/rust/backtrace-raw-normal/src/main.rs`:

```text
c
b
a
```

## Demo 4: StarryOS Memtrack Backtrace

Must contain:

```text
Memory allocation sample recorded
Hard memory allocation sample recorded
BACKTRACE_BEGIN kind=alloc
BT 0 ip=0x...
BACKTRACE_END
STARRY_MEMTRACK_BACKTRACE_OK

=== host backtrace symbolize ===
BACKTRACE_BLOCK ... kind=alloc
```

The `sample_hard` allocation path may show extra frames such as:

```text
starry_memtrack_sample_hard_leaf
starry_memtrack_sample_hard_mid
```

Those frames are useful for inspection, but the stable guest-side assertion is
the deterministic `BACKTRACE_BEGIN kind=alloc` block.

The helper script automatically host-symbolizes the captured app output. When
symbols are available, the symbolized output may also include:

```text
BACKTRACE_BLOCK
starry_memtrack_symbolize_probe
```
