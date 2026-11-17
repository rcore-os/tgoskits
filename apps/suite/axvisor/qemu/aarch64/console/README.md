# AArch64 控制台回归检查

从 `test-suit/axvisor/normal/qemu/aarch64/general` 迁入 `interleave`、`atomic-output`。未加入普通CI、夜间或benchmark；命令由用户触发，后续步骤自动执行。

## 组织与顺序

```text
console/
├── build-aarch64-unknown-none-softfloat.toml  构建配置
├── qemu-aarch64.toml                        一次宿主启动、有序checks
├── guest.toml                              Linux客机，沿用VM3
└── tests/
    ├── prepare-guest.toml                  创建客机
    ├── interleave.toml                     暂停/恢复与日志交错
    ├── prepare-atomic-output.toml          重建客机，恢复启动输出
    └── atomic-output.toml                  连续输出与控制台切换
```

默认 `interleave` → `atomic-output` 共用一次QEMU启动；前置通过 `requires` 声明，同一文件同启只执行一次。保留原4核、8GiB、NVMe/Web UI构建环境，只准备两项检查实际使用的VM3；VM3只有1个vCPU，固定宿主CPU1。`guest.toml`与迁移前VM3配置相同，应用不引用 `test-suit`。

| 检查 | 操作与通过条件 |
| --- | --- |
| interleave | vm list、suspend、resume、echo；保留原关键输出和失败模式 |
| atomic-output | vm show --full、help、list、进入客机观察test pass、客机echo；保留原子上下文睡眠/输出worker停止等失败签名 |

这些是关键输出驱动的回归检查，不逐字节比较全部输出，也不主动强制制造原子上下文。迁移不增强或删除原功能判定。重建VM3是同一次QEMU内的客机操作，不是额外宿主启动。

## 运行

从TGOSKits工作区根目录运行；沿用仓库Rust/QEMU工具链，原Web UI feature需要Node 24及npm 11以上，SDK自动准备受管镜像。

```sh
# 查看场景，不构建、不启动。
cargo xtask axvisor app qemu --app-root apps/suite/axvisor \
  --group qemu --arch aarch64 --list

# 两项共用一次宿主启动。
cargo xtask axvisor app qemu --app-root apps/suite/axvisor \
  --group qemu --arch aarch64 --app aarch64/console

# 单独运行一项及其准备。
cargo xtask axvisor app qemu --app-root apps/suite/axvisor \
  --group qemu --arch aarch64 --app aarch64/console --test-case interleave
cargo xtask axvisor app qemu --app-root apps/suite/axvisor \
  --group qemu --arch aarch64 --app aarch64/console --test-case atomic-output
```

## 增删与调整

`qemu-aarch64.toml`的checks是公开成员和顺序的唯一登记。兼容检查追加在本场景，优先共享启动；仅初始化的文件放tests并由requires引用，不登记成独立功能。更改客机配置、编号或准备时同步所有引用并验证单项/合批；任何成员失败均使整组失败。加入自动测试需另行决定并登记，目录存在不表示已进入CI。
