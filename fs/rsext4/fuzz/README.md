# rsext4 fuzzing

English | [中文](README_CN.md)

This directory holds the `cargo fuzz` targets of `rsext4`. They hand arbitrary bytes to the parsing and mount paths as disk contents and check that, on a malicious or damaged image, those paths only return errors: no panic, no out-of-bounds access and no endless loop.

This directory has its own `[workspace]`, outside the repository workspace, and `cargo xtask` does not build it. It needs a nightly toolchain and `cargo-fuzz` (`cargo install cargo-fuzz`).

## Targets

| Target | Input | Paths covered |
|---|---|---|
| `superblock` | a 1024-byte superblock | decoding, checksum, block size, group count, geometry checks |
| `group_descriptor_table` | a superblock followed by a group descriptor table | decoding each descriptor at the descriptor size and group count the superblock gives |
| `extent_tree` | one extent tree node (the 60 bytes of `i_block` or a whole block) | `ExtentTree::parse_node` |
| `directory_block` | one directory block | the linear entry walk, and the directory block and HTree checksum tails |
| `htree_lookup` | a whole image | after a read-only mount, `lookup_child` of every name in every directory; indexed directories take the HTree lookup |
| `mount_and_walk` | a whole image | a read-only mount, then a bounded walk of directories, metadata, extents, extended attributes and file contents |
| `journal_replay` | a whole image | a read-write mount (replaying the journal when recovery is needed), a read of the root directory, then an unmount |

The walks are bounded (at most 512 inodes, and at most 256 KiB read from each file), so no single input can hold up an iteration with a huge image or a directory cycle.

## Seed corpus

```sh
sh make-seeds.sh            # writes ./corpus/<target>/
```

The script uses `mke2fs` (e2fsprogs 1.47 or later) and a fixed time to build a few small images: 1 KiB and 4 KiB blocks, with and without `metadata_csum`, a large directory indexed by `e2fsck -D`, and an image with one committed journal transaction that needs recovery. The seeds of each parser target are cut from these images.

## Running

```sh
cargo fuzz run -O mount_and_walk corpus/mount_and_walk -- -max_total_time=600 -max_len=600000
```

The whole-image targets need `-max_len` at least the size of the seed image (the `journal_replay` seed is 3 MiB).
