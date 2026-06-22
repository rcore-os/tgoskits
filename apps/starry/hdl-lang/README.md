# hdl-lang — HDL 语言级地毯式测试 (StarryOS app)

面向 StarryOS 的 **HDL 语言/工具链**地毯式测试 app，对应 [#764](https://github.com/rcore-os/tgoskits/issues/764) 的
`verilog <!-- verilator, iverilog, gnumake -->` 与 `bluesv <!-- bluespec systemverilog, system c -->`
两个注释点名项中的语言层 + 综合层。**MODEL A（静态二进制）**：所有仿真在 **构建宿主**用官方工具链编译为
**为 STARRY_ARCH 交叉编译的静态二进制 / Icarus 字节码**，宿主捕获确定性黄金，on-target 逐字节 `cmp`。

六条腿全部通过才打印 `TEST PASSED`（任一不过 → `TEST FAILED`）：

| 腿 | 工具 | on-target 产物 | 语义 |
|:--:|:--:|:--:|:--|
| **VLOG** | Verilator 5.008 | `hdl-tb-vlt`（静态 C++ 二进制） | 综合 SystemVerilog 设计 verilate→C++，musl-cross g++ 静态交叉编译；139 自检，`CARPET_RESULT ALL_PASS` |
| **IVL** | Icarus Verilog 12 | `hdl-tb-ivl.vvp` + 静态 `vvp` | 同一份 SV 设计编为可移植字节码，静态 vvp 运行；**必须逐字节 == VLOG** |
| **BSV** | bsc 2026.01 | `LangBSV.vvp` + 静态 `vvp` | Bluespec SystemVerilog → Verilog → vvp 字节码 |
| **BH** | bsc 2026.01 | `LangBH.vvp` + 静态 `vvp` | Bluespec Classic / Haskell → Verilog → vvp 字节码 |
| **MAKE** | GNU Make 4.4.1 | `lang-make`（静态二进制） | 自包含 Make 语言特性 Makefile（`:=`/`?=`/`+=`/函数/模式规则/自动变量/`.PHONY`/条件/`include`） |
| **yosys** | yosys 0.58 | `yosys_net.vvp` + 静态 `vvp` | 完整综合流程（proc/opt/fsm/memory/techmap）出门级网表，自检 testbench 驱动**综合后网表** + yosys simlib，验证综合结果在 on-target 功能正确 |

> 设计取舍：`verilator`/`iverilog`/`bsc`/`yosys` 本身是 host-only 编译/综合器，没有 on-target 二进制；on-target 跑的是
> 它们的**确定性仿真产物**。yosys 这条腿不是跑综合器，而是仿真它综合出的门级网表（综合结果的功能等价证明）。

## 结构

- `src/rtl/*.sv` + `src/tb/tb_top.sv` —— 综合 SystemVerilog 设计 + 自驱动 testbench（ALU/寄存器堆/计数器/2 个 FSM/桶形移位器/generate/package/枚举/struct/union/打包数组/系统函数；输出确定化的 `TB:` 行，139 自检）。
- `src/LangBSV.bsv` / `src/LangBH.bs` —— 综合 Bluespec 设计（interface/module/method/rule/Reg、ADT/maybe/tuple/vector/FIFO/寄存器堆，确定性 `$display` + `BSV_DONE`/`BH_DONE` 哨兵）。
- `src/make/{Makefile,config.mk}` —— GNU Make 语言特性地毯（确定性输出 + `MAKE_LANG_OK`）。
- `src/yosys/{alu,ctrl,datapath}.v` + `tb_synth.v` —— 综合用 RTL（组合 ALU + Moore FSM + 含同步 RAM 的 datapath）+ 综合后网表自检 testbench（`SYN_DONE`）。
- `vendor/vvp/vvp-<arch>` —— 静态 musl 交叉编译的 Icarus `vvp` 运行时（system VPI + VCD 内嵌；静态二进制不能 dlopen `system.vpi`，故链接进运行时）。驱动 IVL/BSV/BH/yosys 四条字节码腿。
- `vendor/make/make-<arch>` —— 静态 musl 交叉编译的 GNU Make 4.4.1。
- `golden/*.txt` —— 各腿确定性黄金参考（committed；prebuild 在宿主重新捕获装入 overlay 比对）。
- `prebuild.sh` —— 宿主编译全部六条腿、交叉编译/构建静态产物、捕获黄金、装入 overlay。交叉二进制经 `qemu-<arch>-static` 在宿主跑出黄金并自检，故任意宿主上对任意 STARRY_ARCH 都可复现。
- `qemu-<arch>.toml` ×4 —— 跑全部六条腿、各自 `cmp` 黄金，全过才 `TEST PASSED`（`success_regex = ^TEST PASSED$`，`fail_regex` 含 panic 与 `^TEST FAILED$`）。
- `build-<target>.toml` ×4。
- `host-carpets/{verilator-cli,iverilog-cli,bsc-cli,gnumake-cli,yosys-cli,yosys-sta}-carpet.sh` —— verilator / iverilog(+vvp) / bsc(Bluespec) / GNU make / yosys 命令树 + yosys-sta PPA 流程的宿主 CLI 全选项地毯（host-validated 辅助，不参与 on-target 门控，见下文）。

## 运行

```sh
cargo xtask starry app qemu -t hdl-lang --arch aarch64
cargo xtask starry app qemu -t hdl-lang --arch riscv64
cargo xtask starry app qemu -t hdl-lang --arch loongarch64
# x86_64 同 java-lang：apps/starry app-QEMU 受 PVH `-kernel` 加载限制 + 被 CI path-filter 跳过，未在 on-target 独立复核
```

## 工具链

**可复现安装**：`bash apps/starry/hdl-lang/setup-hdl-toolchain.sh`（幂等；按下方钉死版本从源码 / 官方 release 安装 verilator·iverilog·yosys 到 `/usr/local`、bsc 到 `/usr/local/bsc`，并装 musl-cross + qemu-*-static。缺任一工具时 `prebuild.sh` 明确报错而非静默跳过）。
版本：verilator 5.008 / iverilog+vvp 12 / bsc 2026.01（`/usr/local/bsc`）/ yosys 0.58 / GNU make 4.x（均宿主工具）；
musl 交叉 `/opt/<arch>-linux-musl-cross`（riscv64/loongarch64 的 verilator 静态链接需 `-no-pie -fno-pie`，避免
`read-only segment has dynamic relocations`，与 hw4os 既有 HDL case 一致）。VLOG 的 C++ 模型按 TU 分别并行编译再链接
（单条 monolithic g++ 在 loongarch/riscv GCC 上峰值内存/耗时过大，易被 OOM-kill / 超时）。

## 宿主 CLI / EDA 地毯（host-validated auxiliary）

`host-carpets/` 下六份脚本覆盖 HDL 工具链每个工具的 CLI 全选项面（verilator / iverilog+vvp / bsc / GNU make / yosys / yosys-sta），是 **host-validated 辅助**：这些 EDA / 综合 / 构建工具是宿主独占工具，没有在 StarryOS 上直跑的运行时（on-target 门控跑的是这些工具产出的门级网表/可执行的仿真，即上文 6 腿）。这六份脚本不参与 on-target `TEST PASSED` 门控，随 PR 附入供审阅，逐项对官方 `--help` 核对、每步带 `timeout`/`</dev/null`（工具永不阻塞终端）：
- `host-carpets/verilator-cli-carpet.sh` —— verilator 命令/选项全树（`--cc`/`--binary`/`--lint-only`/`--trace`/`-Wall`/`-O3`/`--top-module`/`-I`/`-y`/`--exe` 等 120+ 选项），逐项对 `verilator --help` 核对 → `VERILATOR_CLI_OK`（95 检查）。权威：verilator.org。
- `host-carpets/iverilog-cli-carpet.sh` —— iverilog 编译器（`-o`/`-s`/`-g2012`/`-D`/`-I`/`-y`/`-t`/`-Wall`）+ `vvp` 仿真器（flag + plusargs）全选项 → `IVERILOG_CLI_OK`（40 检查）。权威：github.com/steveicarus/iverilog。
- `host-carpets/bsc-cli-carpet.sh` —— bsc（Bluespec 编译器）`bsc -help` 枚举的 77 选项（`-verilog`/`-sim`/`-systemc`/`-u`/`-g`/`-p`/`-bdir`/`-vdir`/`-Xc`/`-Xl` 等），含 `-systemc` SystemC 后端 → `BSC_CLI_OK 87/87`（+2 reasoned skip）。权威：github.com/B-Lang-org/bsc。
- `host-carpets/gnumake-cli-carpet.sh` —— GNU make CLI 全选项（`-j`/`-C`/`-f`/`-n`/`-k`/`-B`/`-W`/`-o`/`--debug`/`-p`/`-q`/`-s`/`-r`/`-R`/`-l` 等 ~30 选项）→ `GNUMAKE_CLI_OK`（92 检查）。权威：gnu.org/software/make。

- `host-carpets/yosys-cli-carpet.sh` —— yosys 命令/选项树地毯（`read_verilog`/`read_rtlil`、`hierarchy`/`proc`/`opt`/
  `fsm`/`memory`/`techmap`/`flatten`/`abc`/`dfflibmap`/`synth`、`write_verilog`/`write_json`/`write_rtlil`/`write_blif`、
  `stat`/`check`/`select`/`splitnets`/`sat`/`chformal`、`-p`/`-s`/`-q`/`-V` 等调用形式），逐项对官方 `yosys --help` /
  `cmd_ref` 核对，每步带 `timeout` 与 `</dev/null`（综合器永不阻塞终端）。权威：github.com/YosysHQ/yosys。
- `host-carpets/yosys-sta-carpet.sh` —— 逐字复刻 OSCPU/yosys-sta `scripts/yosys.tcl` 的综合配方（`synth -flatten -run :fine`
  →`share -aggressive`/`onehot`/`muxpack`/`opt_demorgan`→`clockgate`/`dfflibmap`→`abc -D <period> -constr <sdc>`
  →`setundef -zero`/`opt_clean -purge`/`autoname`→`tee check -mapped`/`tee stat -liberty`→`write_verilog` 网表），在
  canonical `gcd` 设计 + 自包含 liberty + 真实 SDC 上断言每个 STA 消费输入（liberty 映射网表、面积报告、DRC 检查、
  SDC 解析、liberty 解析）格式良好；外部 iSTA/iPA 时序+功耗运行为带 `make sta` 配方与产出输入的 reasoned skip
  （宿主未装 STA 引擎）。权威：github.com/OSCPU/yosys-sta。

## 宿主自检结论

- x86_64 / aarch64 / riscv64：六条腿宿主自检（交叉二进制经 `qemu-<arch>-static` 跑、各 `cmp` 黄金）**全过 6/6**。
- loongarch64：IVL/BSV/BH/MAKE/yosys 五条腿（静态 `vvp`/`make`）宿主自检全过；唯 **VLOG 腿**的 Verilator `--timing`
  C++20 协程在 `qemu-loongarch64-static`（用户态模拟器）下停滞——这是宿主模拟器限制，非靶上缺陷：二进制为干净
  static-pie，黄金与其余 arch 逐字节相同（确定化），其权威运行在真 StarryOS 全系统 (TCG)。
