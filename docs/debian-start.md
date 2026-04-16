# Debian-bookworm 快速上手指南

StarryOS 启动 Debian-bookworm 教程
## 1. 环境准备

前提是能够[正常启动](docs/quick-start.md) StarryOS Alpine 版本：
 - qemu-aarch64
 - rust

## 2. 下载 Debian 的 rootfs

[Debian-bookworm](https://github.com/rcore-os/tgoskits/releases/download/debian-bookworm-rootfs/rootfs-aarch64.img.tar.gz)
将它解压并替换
target/aarch64-unknown-none-softfloat/rootfs-aarch64.img
查看[init.sh](os/StarryOS/starryos/src/init.sh)，启动 StarryOS 即可：

启动命令`cargo starry qemu --arch aarch64`

```text
starry:~# apt install neofetch
bash: child setpgid (5080 to 5080): No such process
Reading package lists... Done
Building dependency tree... Done
Reading state information... Done
neofetch is already the newest version (7.1.0-4).
0 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.
starry:~# neofetch
bash: child setpgid (5083 to 5083): No such process
       _,met$$$$$gg.          root@starry 
    ,g$$$$$$$$$$$$$$$P.       ----------- 
  ,g$$P"     """Y$$.".        OS: Debian GNU/Linux 12 (bookworm) aarch64 
 ,$$P'              `$$$.     Kernel: 10.0.0 
',$$P       ,ggs.     `$$b:   Uptime: 20558 days, 10 hours, 3 mins 
`d$$'     ,$P"'   .    $$$    Packages: 248 (dpkg) 
 $$P      d$'     ,    $$P    Shell: bash 5.2.15 
 $$:      $$.   -    ,d$$'    Memory: 12986MiB / 31773MiB 
 $$;      Y$b._   _,d$P'
 Y$$.    `.`"Y$$$$P"'                                 
 `$$b      "-.__                                      
  `Y$$
   `Y$$.
     `$$b.
       `Y$$b.
          `"Y$b._
              `"""

starry:~# 
```