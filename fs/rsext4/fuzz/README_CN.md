# rsext4 模糊测试

[English](README.md) | 中文

本目录是 `rsext4` 的 `cargo fuzz` 目标，用于把任意字节作为磁盘内容交给解析和挂载路径，检查它们在恶意或损坏的镜像上只返回错误，不崩溃、不越界、不无限循环。

本目录有自己的 `[workspace]`，不属于仓库工作区，`cargo xtask` 不构建它；它需要夜间工具链和 `cargo-fuzz`（`cargo install cargo-fuzz`）。

## 目标

| 目标 | 输入 | 覆盖的路径 |
|---|---|---|
| `superblock` | 1024 字节超级块 | 解码、校验和、块大小、块组数、几何检查 |
| `group_descriptor_table` | 超级块后接块组描述符表 | 按超级块给出的描述符大小和块组数解码每个描述符 |
| `extent_tree` | 一个 extent 树节点（`i_block` 的 60 字节或一个整块） | `ExtentTree::parse_node` |
| `directory_block` | 一个目录块 | 线性目录项遍历、目录块和 HTree 校验尾 |
| `htree_lookup` | 整个镜像 | 只读挂载后对每个目录逐名 `lookup_child`，索引目录走 HTree 查找 |
| `mount_and_walk` | 整个镜像 | 只读挂载，有界遍历目录、元数据、extent、扩展属性和文件内容 |
| `journal_replay` | 整个镜像 | 读写挂载（需要恢复时重放日志），读根目录后卸载 |

遍历有上限（最多 512 个 inode，每个文件最多读 256 KiB），单个输入不会因镜像过大或目录成环而拖住一次迭代。

## 种子语料

```sh
sh make-seeds.sh            # 写入 ./corpus/<目标>/
```

脚本用 `mke2fs`（e2fsprogs 1.47 或更新）和固定时间生成几个小镜像：1 KiB 和 4 KiB 块、有无 `metadata_csum`、一个由 `e2fsck -D` 建立索引的大目录，以及一个带一条已提交日志事务、需要恢复的镜像。每个解析器目标的种子从这些镜像中截取。

## 运行

```sh
cargo fuzz run -O mount_and_walk corpus/mount_and_walk -- -max_total_time=600 -max_len=600000
```

整镜像目标需要把 `-max_len` 设为不小于种子镜像的大小（`journal_replay` 的种子为 3 MiB）。
