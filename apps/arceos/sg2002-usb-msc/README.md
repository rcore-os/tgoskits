# SG2002 USB大容量存储示例与板测

本程序归入apps，保留**手动展示和原axtest自动判定两个入口**，共用同一份USB实现。旧包名`arceos-axtest-sg2002-usb-msc`为命令兼容保留，不再在test-suit放置复制包或占位目录。目前没有CI/夜测登记。

## 文件

```text
sg2002-usb-msc/
├── Cargo.toml
├── build.rs                              保留axtest cfg检查
├── src/
│   ├── main.rs                           手动应用入口
│   ├── lib.rs                            原命令编码/配置计算及纯逻辑测试
│   ├── usb.rs                            唯一USB/IRQ/MSC实现
│   └── usb/bench.rs                      原读取/可选写入采样
├── tests/axtest.rs                        原目标名称及结果契约
├── build-aka-00-sg2002.toml                原板测构建条件
├── board-aka-00-sg2002.toml                原板测成功/失败条件
├── build-manual-riscv64gc-unknown-none-elf.toml
├── board-manual-aka-00-sg2002.toml
└── README.md
```

`usb-msc`启用共享实现；`axtest`保留原Cargo测试目标；`arceos`启用普通应用二进制及真实Rust std运行时。默认features仍为空。test-suit旧路径已删除，构建配置应使用本目录的新路径。

## 硬件和行为

需要AKA-00-SG2002及可识别、可读取的USB MSC设备（U盘或其他USB存储）。程序接管DWC2控制器和IRQ，初始化/枚举、查询容量、读取数据并打印采样结果；手动入口是人工发起程序，不提供交互shell。

没有USB存储不会报成功：共享实现返回NoMassStorageDevice，两个入口均清楚报告失败。默认只读，原有写入编译选项代码保留但未启用；写入会改动指定介质LBA，当前配置保持原只读验收约束。本迁移不会自动执行写入。

原读取尺寸、吞吐打印、transfer_busy_wait_iters=0检查以及axtest单项/汇总/最终完成条件均保留。未将性能逻辑拆除，也未复制一套实现。

## 手动展示

从tgoskits根目录：

```bash
# 只构建，不申请板卡
cargo xtask arceos build \
  -c apps/arceos/sg2002-usb-msc/build-manual-riscv64gc-unknown-none-elf.toml

# 人工发起板卡运行，显式使用自己的服务地址
cargo xtask arceos board \
  -c apps/arceos/sg2002-usb-msc/build-manual-riscv64gc-unknown-none-elf.toml \
  --board-config apps/arceos/sg2002-usb-msc/board-manual-aka-00-sg2002.toml \
  --server <host> --port <port>
```

手动成功标记为`SG2002_USB_MSC_APP_OK`；错误打印`SG2002_DWC2_MSC_FAIL`并以非零退出。它不打印AXTEST_SUITE_OK冒充测试套件。

## 原自动判定入口

原命令保持：

```bash
cargo xtask ktest board \
  -p arceos-axtest-sg2002-usb-msc --test axtest -b aka-00-sg2002 \
  --server <host> --port <port>
```

ktest通过Cargo metadata从新的apps路径发现同名包、同名harness=false目标及runtime=board；板名优先选择`build-aka-00-sg2002.toml`，不会因新增手动构建配置而误选。原board文件要求读取采样、无忙等及AXTEST套件通过，错误返回Failed，保持自动判定用法。这里只保留既有测试能力，不表示它已加入CI或现在已经实板通过。

## 已知问题与验收界限

迁移前最近一次实板运行卡在USB IRQ接管附近，尚未见测试的INIT_START，最后600秒超时。共享实现当前仍在硬IRQ回调调用handler.handle，该接口包含任务上下文的drain/rearm，与现USB接口契约不符；这是待单独修复、实板对照确认的问题。

本次迁移不擅自调整控制器时序、IRQ行为或放宽原板测成功条件。构建、Clippy及纯逻辑检查不能代替真实USB/板卡验收；README保留已知未通过状态。

维护时修改共享usb模块，两个入口都验证；纯编码/参数计算测试保留在lib.rs末尾。变更结果标记须同时核对两个board配置，旧ktest目标/包名变更须明确迁移命令，不能靠目录复制保持兼容。人工测试只从本机构建，显式选择服务，不修改共享CI配置，结束后确认自己的会话释放。
