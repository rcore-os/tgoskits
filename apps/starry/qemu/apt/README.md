# APT QEMU 冒烟测试

aarch64 和 riscv64 的 Debian APT 冒烟测试使用 `init=/bin/sh` 直接启动
`/bin/sh`。这样可以绕过镜像的 OpenRC 和登录流程；这些流程不属于 APT
行为本身，而且在这两个目标上可能导致提示符或服务启动状态不确定。

直接启动 shell 的路径不覆盖 init 系统启动、服务监管、DHCP 配置和 DNS
配置。只有 Debian 镜像已经提供可用网络配置时，测试环境才是有效的；
`apt-get` 或软件源失败会输出 `APT_HELLO_TEST_FAILED`，不会被判定为通过。
