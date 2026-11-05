# OrangePi 5 Plus 上的 StarryOS 自编应用

宿主机把编译工具链、当前提交的源码和 Cargo 依赖准备成 ext4 镜像。StarryOS 从板卡会话的 HTTP 服务接收镜像，校验后直接挂载，在板端先编译 tg-xtask，再编译 StarryOS。

## 1. 启动应用

`cargo xtask starry app board` 负责宿主机准备、板卡租约、种子内核启动、会话文件上传和串口结果判定。应用运行期间，板端须能访问服务发布的 HTTP 地址。

### 1.1 环境准备

宿主机需要项目固定的 Rust 工具链、Docker、支持 AArch64 的 binfmt/QEMU、Python3、rsync、GNU tar 和 e2fsprogs。`prepare_host_rootfs.sh` 根据 `rootfs/Dockerfile` 缓存 Alpine 3.22 的 ARM64 musl 用户态和 `aarch64-unknown-linux-musl` Rust 工具链；`host-prebuild.sh` 获取锁定依赖，`prepare_image.py` 在宿主机展开源码及公开 crates.io 依赖，并生成完整 ext4 镜像。源码由 `prepare_source()` 通过 `git archive HEAD` 归档；运行前必须提交已跟踪文件的修改，未跟踪文件不会上传。不包含宿主机 Cargo 凭据、配置或其他 Git 仓库。

板卡服务须配置实际串口、启动方式和电源控制，可通过 `cargo xtask board config` 配置服务地址。板端初始根文件系统需要 shell、Python3、curl 或 wget、mount、umount、losetup、chroot、`/dev/shm` 临时文件系统和可写存储；内核需要 ext4、只读 loop 挂载、tmpfs 与匿名页 swap 回收支持。本应用已在 8 GiB 级内存的板卡上完成冷编；可写存储除 `manifest.json` 中 `size` 指定的完整镜像空间外，还须容纳内核 target、日志和最终产物。默认根分区由板卡配置的 `cmdline` 指定；不同存储布局通过标准 `--board-config` 选择完整配置。

本应用仅提供自编流程和会话接入，所需内核兼容性修复独立提交。当前三轮实板验证使用包含这些修复的组合内核，覆盖真实应用入口、网络传输、两阶段冷编和清理；单独 `dev` 加本应用尚未完成同等验证。标准命令的固件传核和启动能力还须由实际板卡服务配置验证。

### 1.2 单条启动命令

在包含应用及所需内核改动的仓库根目录执行以下命令。首次准备会在宿主机安装工具链并生成镜像，后续内容未变化时复用已校验的宿主机资源。

```bash
cargo xtask starry app board -t orangepi-5-plus-selfbuild
```

`prepare_board_session_assets()` 收集 `host-prebuild.sh` 生成的镜像分片和脚本。板卡运行器上传当前会话资源，`init.sh` 使用配置中的 `${sessionFile:usr/share/selfbuild/run.sh}` 取得入口，并在 StarryOS 中启动应用。

## 2. 镜像与编译

`session-selfbuild.sh` 独占本应用的镜像，使用宿主机时间校时，通过 `receive_image.py` 校验并恢复完整镜像，然后挂载镜像与本次运行的外部工作目录。板端不需要展开归档或在线安装编译依赖。

### 2.1 完整性与运行隔离

`bind_board_session_checksums()` 在宿主机展开 `${sessionFileSha256:<relative-path>}`，把本次入口脚本、接收器、清单和校时文件的 SHA-256 写入串口启动命令。`init.sh` 先校验这些 HTTP 下载内容，再执行入口；预期校验值来自宿主机文件，不通过同一 HTTP 下载取得。

`manifest.json` 记录完整镜像及各个不超过 60 MiB 分片的大小和 SHA-256。`receive_image.py` 使用 1 MiB 缓冲写入，逐片检查已有镜像，重新下载缺失或已改变的分片，每片校验并执行 `fsync` 成功后才报告完成，下载、校验或写回失败时从该分片起点重传，最多尝试 3 次；已完成分片不重复下载，最后校验完整镜像。只有全部内容与宿主机镜像一致并写回成功后才允许挂载；镜像上次运行留下的写入不作为已验证缓存接受。

`session-selfbuild.sh` 以只读 loop 和 `mount -o ro` 挂载 SDK。源码、工具链及 registry 保持只读；每次运行使用新的 `/opt/tgoskits-selfbuild-app/runs/<run-id>`，通过 bind 挂载提供独立的 `/work` 和源码工作区 `tmp`。`guest-selfbuild.sh` 在 `/work/cargo-home` 保存 Cargo 锁和使用记录，并通过符号链接访问 SDK 中的 registry，拒绝复用运行目录和 target。镜像恢复、挂载和编译在 `/dev/shm/tgoskits-selfbuild-active` 这一临时应用锁内执行；只有卸载和 loop 释放成功后才释放锁，清理失败时保留锁及现场，避免其他运行覆盖仍在使用的镜像。重启后临时锁与内核挂载一同消失，不遗留持久锁目录。

### 2.2 结果与退出

`guest-selfbuild.sh` 在 StarryOS 内冷编 `tg-xtask`，再调用生成的工具构建 StarryOS。构建配置启用 8 核，使用默认 Cargo 并行度，`CARGO_INCREMENTAL=0` 避免为一次性冷编保存增量状态。默认先把 tg-xtask target 放在 tmpfs，保留生成的可执行文件后立即卸载这块临时存储，释放内存，再用同一份工具在外部 ext4 目录中构建内核。`session-selfbuild.sh` 请求的 `size=4G` 是否作为硬配额执行取决于运行内核，不能视为已强制限制内存占用。

完整日志、源码提交和快照哈希、阶段耗时、tg-xtask、内核 ELF/BIN 以及 SHA-256 保存在板端 `/opt/tgoskits-selfbuild-app/output/runs/<run-id>`。将下列命令中的 `RUN_ID` 替换为串口打印的运行标识，在板端执行可从第一行开始显示完整日志并继续跟随新增内容；`-n +1` 保留已有全部内容，`-f` 持续等待更新。

```bash
tail -n +1 -f /opt/tgoskits-selfbuild-app/output/runs/RUN_ID/run.log
```

`selfbuild_log.sh` 在退出时停止并回收进度进程、等待 `tee` 完整写出日志，再向外层返回构建状态。中间编译产物在阶段完成后清理，日志与最终产物保留。

编译成功、产物校验记录生成、所有挂载和 loop 设备清理成功后，`session-selfbuild.sh` 才输出运行器接受的 `STARRY-ORANGEPI5PLUS-SELFBUILD-SESSION-PASS`。失败返回非零状态并保留日志，正常结束后留在 StarryOS。`tests/script_smoke.sh` 检查脚本、冷编目录约束以及真实 HTTP 镜像接收、复用、恢复与错误拒绝；完整板端编译结果以当前提交对应的运行日志为准。
