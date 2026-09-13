---
name: board-linux-starry-debug
description: 调试需要先通过本地板卡服务向会话传送文件，或先从 Linux 侧检查实体板卡，再运行 StarryOS 或 ArceOS 的流程。目标系统具备可用网络驱动时优先使用会话文件；只有需要持久状态或目标系统无法取得会话文件时，才写入 Linux 根文件系统。
---

# 板卡 Linux 与 Starry 调试

## 适用范围

在同一次实体板卡租约内需要完成下列工作时使用本流程：

1. 启动板卡的常规 Linux 映像。
2. 从串口或 Linux 命令取得板卡网络地址。
3. 通过安全外壳协议或 `rsync` 把文件写入 Linux 根文件系统。
4. 把文件系统修改同步到存储设备。
5. 释放板卡租约。
6. 再次取得板卡并运行 StarryOS 或 ArceOS。

本流程用于避免一种常见错误：文件在 Linux 中刚复制完成，尚未可靠写入存储设备就重启到 StarryOS，导致下一次启动看不到新文件。

## 文件传送方式

目标内核具备可用网络驱动，且客户机内有 `curl` 或 `wget` 时，优先使用板卡会话文件：

1. 通过 Starry 板卡用例或应用叠加层声明或生成文件。
2. 由 `BoardSession::upload_shared_file` 上传到当前会话。
3. 在 `shell_init_cmd` 或 `init.sh` 中下载 `${sessionFile:<relative-path>}`。
4. 使用有界重试，因为命令行提示符出现时，网络连接或动态主机配置协议路由可能尚未就绪。

测试程序、脚本、测试夹具和其他临时文件默认采用此方式。它能隔离各次运行，也不会修改共享根文件系统。

仅在满足下列至少一项时使用后文的 Linux 部署流程：

- 目标系统没有网络驱动，或无法路由到板卡服务；
- 工作负载明确验证 Linux 与 Starry 共享根文件系统的持久状态；
- 文件必须在板卡会话结束后继续存在；
- 用户要求的证据本身包含 Linux 侧检查、软件包安装或 Linux 冒烟测试。

如果会话下载只是早于网络启动，不要立即改用安全外壳协议或 `rsync`。先加入有界重试，并在最终下载失败时输出明确的失败标记。

## 本地板卡服务

除非用户明确给出共享服务地址，否则优先使用仓库的本地板卡服务：

```bash
cargo xtask board ls
cargo xtask board connect -b OrangePi-5-Plus
```

请求的板卡类型不存在时，先列出可用类型并使用本地服务给出的精确名称。例如，共享服务可能使用 `OrangePi-5-Plus-robot`，本地服务则可能只提供 `OrangePi-5-Plus`。

`board connect` 会持有租约，直到外层进程退出。在串口中的 Linux 命令行退出登录，并不一定会释放板卡。

## Linux 部署流程

只有前述选择条件成立时才执行本流程。

1. 保持 `board connect` 运行，直到 Linux 出现登录提示符或命令行提示符。
2. 从启动信息取得网络地址，或运行：

```bash
ip -brief addr
```

3. 在另一个宿主机命令中验证安全外壳协议连接：

```bash
ssh -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout=8 orangepi@<ip> \
  'hostname; id; ip -brief addr'
```

4. 先把文件复制到 `/tmp`，再以管理员权限替换最终路径并同步：

```bash
rsync -az --delete <local-dir>/ orangepi@<ip>:/tmp/<name>/
ssh orangepi@<ip> '
  set -e
  printf "%s\n" orangepi | sudo -S rm -rf /target/path
  printf "%s\n" orangepi | sudo -S mv /tmp/<name> /target/path
  printf "%s\n" orangepi | sudo -S chown -R root:root /target/path
  printf "%s\n" orangepi | sudo -S sync
  ls -l /target/path
  sync
'
```

工作负载能在 Linux 上运行时，部署后执行一次 Linux 冒烟测试，并保存精确成功标记或最终结果行。

## 释放租约

启动 StarryOS 或 ArceOS 板卡运行前，结束 `board connect` 进程：

```bash
ps -ef | rg 'target/debug/tg-xtask board connect|cargo xtask board connect'
kill <pid>
sleep 2
cargo xtask board ls
```

只有板卡重新显示为可用时才继续。

## StarryOS 板卡运行

通过 `cargo xtask` 运行板卡工作负载。除非必须使用远程服务地址，否则使用本地板卡类型：

```bash
cargo xtask starry app board -t <case> --board-config <config> -b OrangePi-5-Plus
```

如果应用板卡配置定义了自己的 `shell_init_cmd`，核验运行器确实使用该字段。如果实际命令来自应用的 `init.sh`，先检查 `scripts/axbuild/src/starry/mod.rs` 和应用运行路径，不要直接认定配置错误。

## 诊断 StarryOS 的 `not found`

Linux 中存在某路径，但 StarryOS 报告 `<binary>: not found` 时，不要直接认定程序构建失败。先检查 StarryOS 可见的根文件系统。

诊断配置优先放在仓库外；若临时放入仓库，提交前删除：

```toml
board_type = "OrangePi-5-Plus"
shell_prefix = "root@starry:/root #"
shell_init_cmd = '''
echo BOARD_DIAG_BEGIN
cd /target/path
pwd
ls -ld /target/path
ls -l /target/path
ls -l /lib/ld-linux-aarch64.so.1 /lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 2>&1 || true
od -An -tx1 -N64 /target/path/<binary> 2>&1 || true
readelf -l /target/path/<binary> 2>&1 || true
echo BOARD_DIAG_DONE
'''
success_regex = ["(?m)^BOARD_DIAG_DONE$"]
fail_regex = ["(?i)\\bpanic(?:ked)?\\b"]
timeout = 120
```

按以下证据判断：

- 目录存在但程序不存在：从 Linux 重新部署，执行 `sync` 后再释放租约。
- 程序存在但 `PT_INTERP` 指定的动态加载器不存在：安装或复制动态加载器和所需共享库，或针对客户机已有运行环境重新构建。
- 客户机内没有 `readelf`：用 `od` 和 `ls` 提供最小证据，再从 Linux 或宿主机使用 `readelf` 检查可执行与可链接格式文件。

## 保留证据

只保留足以支持结论的证据：

- 板卡类型，以及使用本地服务还是共享服务；
- Linux 网络地址和部署目的路径；
- Linux 冒烟测试结果行或成功标记；
- StarryOS 最终结果行或成功标记；
- 已定位的根因，尤其是陈旧的根文件系统内容、缺失的动态加载器或运行器命令覆盖。

除非诊断配置将成为长期维护的项目测试项，否则提交前删除临时配置。
