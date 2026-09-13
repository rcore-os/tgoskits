---
name: board-uboot-fsck-repair
description: 通过 `ostool-server` 中断 U-Boot，临时注入 `extraboardargs=fsckfix`，启动 Linux 并确认登录、管理员或用户命令行提示符，以修复远程实体板卡的 Linux 第四扩展文件系统根文件系统。适用于初始内存文件系统的文件系统检查因日志、孤儿项或索引节点损坏而阻止启动，StarryOS 或 ArceOS 板卡测试可能损坏 OrangePi-5-Plus 根文件系统，用户要求通过 U-Boot 修复文件系统，或需要在 Starry 板卡文件系统测试前后确认 Linux 能正常启动。
---

# 通过 U-Boot 修复板卡文件系统

## 概述

通过一次性的 U-Boot 环境变量覆盖修复板卡根文件系统，然后证明板卡能够进入 Linux，再继续板卡测试。为保证过程可重复，优先使用附带脚本；交互串口更可靠时再改用手工 `ostool board connect` 流程。

## 快速命令

从仓库根目录运行：

```bash
node .agents/skills/board-uboot-fsck-repair/scripts/uboot_fsck_repair.js \
  --board-type OrangePi-5-Plus
```

脚本默认读取 `~/.ostool/config.toml`。需要指定其他服务时使用：

```bash
node .agents/skills/board-uboot-fsck-repair/scripts/uboot_fsck_repair.js \
  --server 10.3.10.9 --port 2999 --board-type OrangePi-5-Plus
```

只有同时满足下列条件才算成功：

- 已进入 U-Boot 提示符；
- 已发送 `setenv extraboardargs fsckfix` 和 `boot`；
- Linux 已进入登录提示符、管理员命令行或自动登录用户命令行，例如 `orangepi@orangepi5plus:~$`；
- 脚本已输出 `RESULT ... linux_login=true ...`，并保存串口日志。

## 手工流程

1. 检查板卡是否可用：

```bash
ostool board ls
```

2. 连接板卡并中断 U-Boot：

```bash
ostool board connect -b OrangePi-5-Plus
```

控制台显示 `Hit any key to stop autoboot:` 时按空格键。

3. 在 `=>` 提示符中注入修复参数，但不保存环境变量：

```text
setenv extraboardargs fsckfix
boot
```

不要执行 `saveenv`，此流程只修复本次启动。在 Orange Pi 映像上优先使用 `extraboardargs=fsckfix`，不要使用 `extraargs=fsck.repair=yes`：`orangepiEnv.txt` 可能覆盖 `extraargs`，而启动脚本会在稍后追加 `extraboardargs`。

4. 确认初始内存文件系统执行强制修复。可接受证据包括 `fsck.ext4 -y -C0 /dev/mmcblk0p2`、`FILE SYSTEM WAS MODIFIED`、已修复或清除的条目，或后续干净检查。
5. 只有 Linux 出现 `root@...#`、`<host> login:` 或 `orangepi@orangepi5plus:~$` 等提示符时才继续。如果仍显示 `UNEXPECTED INCONSISTENCY; RUN fsck MANUALLY`，保存串口日志，不要在该根文件系统上运行 Starry 板卡测试。

## 板卡测试前后检查

破坏性板卡验证前执行一次修复；StarryOS 写入 Linux 根文件系统后再检查一次：

1. 按本技能修复并证明 Linux 能启动。
2. 运行 Starry 板卡工作负载，优先使用 `cargo xtask starry test board ...`。若只需最小根文件系统安全检查，在仓库外创建临时配置：

```toml
board_type = "OrangePi-5-Plus"
shell_prefix = "root@starry:/root #"
shell_init_cmd = "echo STARRY_MINIMAL_BOOT_OK"
success_regex = ["(?m)^STARRY_MINIMAL_BOOT_OK\\s*$"]
fail_regex = ["(?i)(kernel panic|panicked at|fatal exception)"]
timeout = 180
```

然后运行：

```bash
cargo xtask starry test board -t smoke-orangepi-5-plus \
  --board-test-config /tmp/starry-minimal-orangepi-5-plus.toml
```

3. 不带 `fsckfix` 正常启动 Linux，检查初始内存文件系统的文件系统检查是否报告损坏：

```bash
cargo xtask board connect -b OrangePi-5-Plus
```

4. 通过条件：Starry 到达 `root@starry:/root #` 并输出成功标记；Linux 正常启动到登录或用户命令行，并且未输出 `directory corrupted`、`UNEXPECTED INCONSISTENCY` 或需要手工文件系统检查的消息。
5. Linux 正常启动可能输出 `recovering journal`，运行 `fsck.ext4 -a` 并以状态码 `1` 结束。这表示错误已自动修复；只有启动继续到提示符，且没有手工检查消息时才可接受。
6. Linux 文件系统检查失败时，保存失败串口日志，释放串口会话，再次执行本技能后才把板卡归还资源池。
7. Starry 可能已经到达命令行提示符，但测试命令仍失败或超时。即使如此，也必须执行 Linux 正常启动检查后才能判断根文件系统安全。

## 失败处理

- 未能中断 U-Boot 时，释放会话后重试；重新连接会从干净会话重新给板卡上电。
- 脚本无法连接串口网络套接字时，检查 `ostool board ls` 和 `~/.ostool/config.toml` 中 `[board] server_ip`、`port` 的值。
- 板卡已进入 Linux 但脚本没有识别登录状态时，使用与实际控制台提示符匹配的 `--login-regex '<pattern>'` 重新运行。
- 修复参数没有影响初始内存文件系统时，检查 `/boot.cmd` 和 `/boot/orangepiEnv.txt`；已知 Orange Pi 映像使用 `extraboardargs=fsckfix` 才有效。
- 收集完证据后始终释放板卡会话。串口网络套接字关闭或会话释放时，服务会关闭板卡电源。

## 脚本接口

`scripts/uboot_fsck_repair.js` 使用与 `ostool board connect` 相同的 `ostool-server` 接口：

- `POST /api/v1/sessions`；
- `ws_url` 指定的串口网络套接字；
- 持有会话期间发送心跳；
- 退出时调用 `DELETE /api/v1/sessions/<id>`。

查看全部选项：

```bash
node .agents/skills/board-uboot-fsck-repair/scripts/uboot_fsck_repair.js --help
```
