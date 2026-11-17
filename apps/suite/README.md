# 手动测试套件

这里承接从 `test-suit` 移出的、未加入 CI 的测试场景。目录存在不代表自动执行；本目录当前不登记到普通 CI、夜间或 benchmark 清单。

按操作系统、运行方式、架构或板型、启动场景组织：

```text
suite/
└── axvisor/
    ├── board/orangepi-5-plus/robot-linux-perf30/
    └── qemu/aarch64/
        ├── console/                        两项控制台检查，同启执行
        ├── http-control-plane/             人工API访问
        ├── web-ui-demo/                    人工网页演示
        └── browser-console-3vm/            三客户机控制台与日志调试
```

一个运行配置对应一次宿主启动，同一场景可以保留普通/调试变体。构建、板卡或 QEMU 配置与 `guest.toml` 放场景根目录，启动后检查放 `tests/<用途>.toml`，由运行配置的 `[axbuild].checks` 按顺序引用。环境兼容的新增检查优先复用已有启动；不能共享时另建场景并说明原因。不得用目录整理降低原有负载或通过门槛。

AxVisor 使用显式应用根目录，复用原应用执行器：

```sh
cargo xtask axvisor app board --app-root apps/suite/axvisor \
  --group board --list
cargo xtask axvisor app board --app-root apps/suite/axvisor \
  --group board --app orangepi-5-plus/robot-linux-perf30
```

`--app-root` 也适用于 QEMU 应用和外部项目自己的应用目录，不要求该目录位于 TGOSKits 内。未指定时仍使用原 `apps/axvisor` 应用入口。

迁入时更新构建中的客户机路径、检查引用、脚本与资产引用及使用文档；确认原 CI 没有消费者，再移除旧入口。接入自动测试需要单独登记，不能把手动目录迁移当作运行验收。

手动选单项时，在`--app <场景路径>`后加`--test-case <检查文件名>`；多个名称可逗号分隔。公共准备继续位于tests，通过requires显式引用，同启执行一次。默认全部执行仍使用checks顺序。

QEMU控制台检查：

```sh
cargo xtask axvisor app qemu --app-root apps/suite/axvisor \
  --group qemu --arch aarch64 --app aarch64/console
```

详见[控制台应用README](axvisor/qemu/aarch64/console/README.md)。`interleave`与`atomic-output`仅由此显式应用根发现，不再登记于test-suit/general。

交互演示使用 `cargo xtask axvisor qemu -c <build> --qemu-config <runtime> --arch aarch64`，不以进程启动或发现列表作为测试通过；只有声明了自动检查的场景才使用上面的应用测试命令验收。原manual已迁入，具体命令及当前限制见[手动QEMU场景](axvisor/qemu/aarch64/README.md)。
