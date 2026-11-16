# 手动测试套件

这里承接从 `test-suit` 移出的、未加入 CI 的测试场景。目录存在不代表自动执行；本目录当前不登记到普通 CI、夜间或 benchmark 清单。

按操作系统、运行方式、架构或板型、启动场景组织：

```text
suite/
└── axvisor/
    └── board/orangepi-5-plus/robot/robot-linux-perf30/
```

一个场景对应一次宿主启动。构建、板卡或 QEMU 配置与 `guest.toml` 放场景根目录，启动后检查放 `tests/<用途>.toml`，由运行配置的 `[axbuild].checks` 按顺序引用。环境兼容的新增检查优先复用已有启动；不能共享时另建场景并说明原因。不得用目录整理降低原有负载或通过门槛。

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
