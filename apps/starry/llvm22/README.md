# llvm22 - LLVM 22 工具链地毯式测试

工业级、精确断言（exact-assertion）的 LLVM 22 工具链正确性测试：由 **musl 原生 LLVM 22.1.x**
（Alpine `apk add llvm22 clang22 lld22 g++ ...`）在 StarryOS 四个架构（x86_64 / aarch64 / riscv64 /
loongarch64）上运行真实的工具，针对一组 LLVM IR / C / C++ 测例，覆盖工业级测试的七个维度（文档逐项、
`--help`/`--version`、逐选项单测、功能输出、边界输入、异常处理、集成流水线），每项一条精确断言。
**不使用交叉编译产物替代**: `lli`、`llc`、`opt`、`clang`、`clang++`、`lld`、`llvm-ar` 等本体都在
StarryOS 上真跑。

| 工具 | 覆盖 | 断言数 |
|:--|:--|:--:|
| `llvm-config` | `--version` 主版本号 = 22；`--targets-built` 含 X86 / AArch64 / RISCV / LoongArch（交叉代码生成能力） | 5 |
| 各工具 `--version` / `--help` | `llvm-as`/`llvm-dis`/`llvm-link`/`llvm-nm`/`llvm-objdump`/`llvm-ar`/`opt`/`llc`/`lli` 各自 `--version` 报告 `LLVM version 22.x`；`opt --print-passes` 列出 `mem2reg`+`instcombine`；`llc --version` 列出 aarch64+riscv64 目标；`FileCheck --help` 退出 0 | 12 |
| `llvm-as` / `llvm-dis` | IR→bitcode（校验 `BC C0 DE` 魔数）；bitcode→IR（含 `@main`）；反汇编产物可重新汇编（往返稳定）；空模块（边界）仍产出合法 bitcode | 4 |
| `lli`（JIT） | JIT 执行 `.ll`：hello 精确 stdout；算术表达式退出码；1..100 循环求和 = 5050；递归 `fib(10)` = 55；1000 条 add 链大 IR（边界）退出码 = 1000 & 255 = 232 | 5 |
| `llc` | IR→本机汇编（`.globl main`）；IR→本机 ELF 目标（ELF 魔数 + `llvm-nm` 列出 `T main` + `llvm-objdump` 反汇编）；四个已构建目标交叉代码生成（`-mtriple=x86_64`/`aarch64`/`riscv64`/`loongarch64` 产物的 ELF `e_machine` 精确等于 62/183/243/258） | 8 |
| `opt` | `-passes=mem2reg` 消除全部 alloca；`-O0` 保留 alloca（不优化）；`-O1`/`-O2`/`-O3`/`default<O2>` 提升为 SSA；`-passes=instcombine` 将 `(x*1)+0` 折叠为 `x`；`-passes=inline` 内联唯一调用点；`llvm-as \| opt \| llvm-dis` bitcode 流水线 | 10 |
| `llvm-link` | 合并两个模块（`@funcA` + `@funcB` 同时出现） | 1 |
| `llvm-ar` | `rcs` 建静态库（`!<arch>` 魔数）；`t` 列出成员；`llvm-nm` 读取库符号索引（funcA + funcB）；`x` 抽取成员 | 4 |
| `FileCheck` | 正例匹配（退出 0）+ 反例（缺失模式必须失败，退出非 0，证明其真的在判别） | 2 |
| `clang` | `--version` = 22.x；C→IR（`-emit-llvm`，含 `@main` + `@printf`）；`-std=c11`/`-std=gnu17` 方言；C→可执行文件（编译+链接+运行）；`-O2 -lm`（`sqrt(2)`） | 6 |
| `clang++` | `--version` = 22.x；C++→IR（Itanium name mangling，`_ZN7Counter…`）；C++→可执行文件（模板 + 类 + STL `std::vector`/`std::string`，编译+链接+运行） | 3 |
| `lld` | `ld.lld --version` = 22.x；`clang -fuse-ld=lld` 链接并运行 | 2 |
| 异常处理 | 畸形 IR 被 `llvm-as`/`llc` 拒绝（退出非 0）；未知 pass 名被 `opt` 拒绝；缺失输入文件被 `lli` 拒绝 | 4 |
| 集成流水线 | `clang -emit-llvm \| opt -O2 \| llc \| clang -fuse-ld=lld` 端到端编译运行；`opt -O2` 优化后的 IR 回灌 `lli` 仍算出 SUM=5050 | 2 |

断言全部针对**与补丁版本无关的稳定不变量**（精确整数、闭式代数、bitcode 魔数、ELF `e_machine`、
Itanium 符号名、固定 stdout 字符串），因此宿主参照工具与目标端 Alpine 工具（22.1.8）给出逐字相同的结果。
`programs/run-llvm22.sh` 依次执行全部 68 项检查，仅当全部通过时才打印 `LLVM22_OK=68/68` 与
`TEST PASSED`（尾部锚点仅在脚本内出现，success_regex 不会自匹配启动命令）；任一失败则打印 `TEST FAILED`。
**不允许跳过**。

## 运行

```
cargo xtask starry app qemu -t llvm22 --arch x86_64
cargo xtask starry app qemu -t llvm22 --arch aarch64
cargo xtask starry app qemu -t llvm22 --arch riscv64
cargo xtask starry app qemu -t llvm22 --arch loongarch64
```

`prebuild.sh` 通过 qemu-user-static 把 base Alpine rootfs 解到 staging 树后，将其 apk 仓库指向
Alpine **edge**（`llvm22` 22.1.x 只在 edge/main 分支，且四个架构齐全），`apk add`
`llvm22 llvm22-dev llvm22-test-utils clang22 lld22 gcc g++ musl-dev binutils`, 由 apk 为目标架构解析
**当前版本**及其完整的 musl 原生 `.so` 闭包（无写死会漂移的 apk URL，无缓存缺失即退出）。随后脚本从
apk 的 installed 数据库里精确取出**本次事务新装/升级的那批包**所拥有的文件（LLVM/Clang 工具本体 +
`libLLVM.so.22.1` 等），复制进 per-app overlay。base 镜像（Alpine 3.23.4）本身已带
`gcc`/`binutils`/`musl-dev`（crt + 链接器），供 `clang` 的 C→可执行链接路径使用；apk 行仍显式列出这些，
以便某个缺失它们的 base 会把它们拉入被复制的事务增量。`g++` 提供 C++ 标准头文件与 `libstdc++`，
供 `clang++` 的 STL 路径使用。

LLVM 闭包体积很大（安装后约 760 MiB），prebuild 在 harness 注入 overlay 之前先把 per-app rootfs 镜像
扩容（truncate + e2fsck + resize2fs），以免 debugfs 注入时静默截断大型 `.so` 文件。

Alpine 的 `llvm22` 包把未加版本后缀的工具（`lli`/`llc`/`opt`/`llvm-config`/`FileCheck`/`llvm-*`）装到
`/usr/lib/llvm22/bin`，只有 `clang`/`clang-22`/`ld.lld` 落在 `/usr/bin`；`run-llvm22.sh` 因此把
`/usr/lib/llvm22/bin` 加入 `PATH`。测例源码位于 `src/`，注入到 `/root/llvm22/src`。

## 架构说明

- x86_64：`-cpu Haswell` 向用户态暴露 XSAVE/AVX2，`lli`/`llc` 的 host-CPU 代码生成路径据此选择向量特性；
  内核侧 CR4.OSXSAVE 与 XCR0 的开启在 dev 分支中。`-m 4096M` 给 `libLLVM`（映射约 190 MiB）与 `clang`
  留出工作集余量。
- aarch64：`-cpu max`（完整 AArch64 特性集，含 LSE 原子与 ARMv8.2+ 扩展）。
- riscv64：`-cpu rv64`。
- loongarch64：`-machine virt -cpu la464`，动态平台（`build-loongarch64*.toml` 带
  `ax-driver/serial`，`uefi=false` / `to_bin=true` 为动态平台的裸二进制引导路径；不再使用已退休的静态
  LoongArch 写法）。

TCG 模拟下运行 LLVM 工具较慢，qemu toml 的 `timeout` 相应放宽。
