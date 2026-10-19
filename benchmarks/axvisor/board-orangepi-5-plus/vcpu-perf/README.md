# Orange Pi vCPU 性能回归测试

本用例在 Orange Pi 5 Plus 上运行单 vCPU ArceOS 客户机，检查周期唤醒与宿主任务竞争下的计算吞吐。客户机自己判定性能，宿主侧由 `axvm` 的 `vcpu_perf` axtest 目标断言判定结果，板卡 runner 只等待内核套件的最终标记，不执行宿主 Python 判定。

## 1. 测量与验收

`guest/src/main.rs` 包含固定负载、采样和最终判定；`baseline.toml` 是基线及门槛的唯一来源。

### 1.1 客户机判定

每块计算执行 256 轮固定整数运算，使用 `black_box()` 保留计算并检查已知答案。另一线程每次休眠 1 ms 后计数。预热 3 秒，再测五个约 3 秒窗口；采样完成后检查耗时、非零工作量/校验值和实际唤醒频率，再取吞吐量中位数与门槛比较。

`guest/build.rs` 从配置生成常量，基线变化会触发客户机重新构建。零或非有限基线、非法回退比例及无效唤醒门槛在构建阶段拒绝。客户机完整且达标输出 `VCPU_PERF_PASS`；性能或负载不合格输出 `VCPU_PERF_FAIL`，随后保持存活以便串口排空。`VCPU_PERF_DONE` 只是采样结束，不是 CI 成功条件。这些客户机标记现在由测试目标读取并断言，板卡运行配置不再直接匹配它们，成功证据统一为内核套件的 `AXTEST_SUITE_OK`。

### 1.2 宿主负载

宿主竞争线程固定 A55 CPU 0，执行有限批次计算后主动让出；`vm.toml` 将单 vCPU 放在同核。负载安装亲和性并完成启动屏障后，才启动客户机。它不再自行运行 60 秒后停止，而是在测试期间持续运行；生命周期结束或线程提前退出输出失败标记。整次板卡执行受 `timeout = 120` 限制，后续运行会重新启动板卡。

负载与客户机的先后顺序现在由 `vcpu_perf` 测试目标掌握：它用 `axvm::VmManager::create()` 接收的 `VmCreatePlan` 建立客户机、启动宿主负载并在客户机结束后比对判定，宿主内核不再为基准保留专用 feature，板卡配置也不再驱动 AxVisor shell。缺失负载就绪、负载提前退出、panic 或客户机 FAIL 都会让测试目标失败，板卡 run 只等待最终的 `AXTEST_SUITE_OK`。

## 2. 标准入口

CI、单项与手工运行共用标准板卡命令。需要仓库固定 Rust 工具链、AArch64 musl/LLVM 工具及已配置的板卡服务，不需要 Python 性能判定器或预先构建客户机。

### 2.1 运行命令

从仓库根目录执行一次命令，自动构建客户机与宿主、把资源装入 AxVisor 宿主 initramfs 并运行测试：

```bash
cargo xtask ktest board -p axvm --test vcpu_perf -b orangepi-5-plus \
  --config benchmarks/axvisor/board-orangepi-5-plus/vcpu-perf/build-aarch64-unknown-none-softfloat.toml \
  --board-config benchmarks/axvisor/board-orangepi-5-plus/vcpu-perf/performance/board-orangepi-5-plus-vcpu-perf.toml
```

同型号板卡可通过 `--board-type <注册名称> --server <服务地址> --port <端口>` 覆盖连接参数；`-b` 只用于板卡名称推导与资源分组，真正生效的构建与运行配置由两个显式路径给出。没有新增 U-Boot 入口适配，也没有外部 `run.sh` 或 `suite_preserve_command` 补偿逻辑。

### 2.2 构建准备

`performance/guest-builds.toml` 只有一项标准构建配置列表：

```toml
arceos_build_configs = ["../guest-build.toml"]
```

`guest-builds.toml` 与板卡运行配置位于同一目录，因此准备步骤在运行配置旁被发现并复用；包名、目标、核数及 `to_bin` 统一保存在 `guest-build.toml`。axbuild 的 `guest_build::prepare()` 复用已有 ArceOS 配置解析、请求准备和 Cargo 构建。未声明该文件的用例不增加准备操作。

## 3. 基线与证据

本测试没有 HTTP、网络控制台、磁盘根文件系统、摄像头、NPU 或机器人电机负载；客户机资源来自宿主 initramfs。它保护计算吞吐，不等同于机器人 FPS、抓取成功率或所有跨核中断路径。

### 3.1 固定门槛

沿用 2026-09-14 基于主线 `c1f5be7373ad2e282fc7e0ff987959971d55cb8a` 加本 PR 修复标定的 `364822 blocks/s`。允许回退 10%，最低 `328339.80`；实际唤醒频率至少 50 次/秒。三次参考采样为 `364822.73、364474.50、365334.20`，极差约 0.24%。

`reference_*` 记录判定迁入客户机之前的参考镜像与标定结果，不是每次被测镜像身份。迁移后继续使用同一门槛验证，不自动调整基线；判定只在采样结束后执行。修改板卡、固件、频率策略或计算负载时需重新评估可比性。
