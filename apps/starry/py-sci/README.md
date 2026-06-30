# py-sci — Python 科学计算地毯式测试

工业级、精确断言（exact-assertion）的科学计算库正确性测试：由 **musl 原生 CPython3** 在 StarryOS
四个架构（x86_64 / aarch64 / riscv64 / loongarch64）上运行，覆盖五个科学计算库：

| 模块 | 库 | 维度 | 标记 |
|:--|:--|:--|:--|
| numpy | NumPy | 数组运算 / 广播 / 视图与跨步 / dtype 转换 / 布尔与花式索引 / 轴向归约 / 线性代数（det·solve·inv·eigvalsh·norm·matmul）/ FFT / PCG64 原始随机流 / 排序与拼接 | `NUMPY_DONE` |
| opencv | OpenCV (cv2) | 色彩空间转换 / 最近邻缩放 / 阈值 / PNG·BMP 往返 / 仿射平移与旋转矩阵 / 边框 / 翻转与转置 / 形态学腐蚀膨胀 / filter2D 单位核 / 积分图 / minMaxLoc / 轮廓计数 | `OPENCV_DONE` |
| pyarrow | Apache Arrow / Parquet | Parquet 写入→读回的精确往返（schema 字段名与类型 + 全部取值）/ compute 计算核 / bool·list 类型 / RecordBatch / ChunkedArray / 内存 IPC 流往返 | `PYARROW_DONE` |
| scipy | SciPy | LU / Cholesky / solve / det / inv / 稀疏 CSR 矩阵 / 凸二次极小化与多项式求根 / 卷积与相关 / 高斯 cdf·pdf / 皮尔逊相关 | `SCIPY_DONE` |
| sympy | SymPy | 符号化简与三角恒等式 / 展开与因式分解 / 方程求解 / 微分·积分·极限 / 精确有理数 / 矩阵行列式与逆 / 求和闭式 / 高精度数值（π、√2 固定前缀） | `SYMPY_DONE` |

每个模块都是自包含地毯（独立 ok/fail 计数器），断言全部采用与库版本无关的**精确整数 /
闭式代数 / 定点整数运算的 sha256 / 标签无关聚类 / 固定位数高精度前缀**等稳定不变量——因此宿主参照库与
（更新的）目标端库会给出逐字相同的结果，不依赖浮点 repr、默认 dtype 宽度或打印格式。模块仅在内部 fail
计数为 0 时打印其 `*_DONE` 标记；`run_pysci.py` 运行全部五个模块，且仅当五个全部通过时才打印
`PY_SCI_OK=5/5` 与 `TEST PASSED`（不允许跳过）。

## numba（暂缓项 🕊）

`numba` 未纳入本测试：Alpine 没有 `py3-numba` 的 musl apk，且其依赖的 `llvmlite` / LLVM JIT
没有 musl 发行版（无法在 musl 原生 CPython 上即时编译）。`run_pysci.py` 会就此打印一条信息性
SKIP 行，**不计入** 通过/总数。这是发行版与工具链的客观边界，并非 StarryOS 的缺陷。

## 运行

```
cargo xtask starry app qemu -t py-sci --arch x86_64
cargo xtask starry app qemu -t py-sci --arch aarch64
cargo xtask starry app qemu -t py-sci --arch riscv64
cargo xtask starry app qemu -t py-sci --arch loongarch64
```

`prebuild.sh` 通过 qemu-user-static 把 base Alpine rootfs 解到 staging 树后，将其 apk
仓库指向 Alpine **v3.23** 分支（main + community），`apk add`
`python3 py3-numpy py3-opencv py3-pyarrow py3-scipy py3-sympy`——由 apk 为目标架构解析**当前版本**
及其完整的 musl 原生 `.so` 闭包（无任何写死的、会漂移的 apk URL，无缓存缺失即退出）。v3.23 为
python 3.12 系列，与 base 镜像同系，无 musl/ABI 漂移；四个架构在 v3.23 上均有这五个包的 musl 原生构建
（`py3-sympy` 为 noarch 纯 python）。脚本随后把解释器、其共享库闭包、标准库 + site-packages
（numpy/cv2/pyarrow/scipy/sympy 及其扩展 `.so`）以及扩展所需的每个 `/usr/lib/*.so*`
（OpenBLAS / libgfortran / OpenCV 库 / Arrow C++ 与 Parquet 库等）复制进 per-app overlay。

科学计算闭包体积很大，prebuild 在 harness 注入 overlay 之前先把 per-app rootfs 镜像扩容
（truncate + e2fsck + resize2fs），以免 debugfs 注入时静默截断大型 `.so` 文件。

地毯源码位于 `python/`；on-target 启动器 `programs/run-pysci.sh` 设置 musl 加载器搜索路径并把
BLAS/OpenMP 线程数固定为 1，然后执行 `python3 /root/pysci/run_pysci.py`。

## 架构说明

- x86_64：`-cpu Haswell` 向用户态暴露 AVX2/AES-NI/XSAVE（NumPy/OpenCV/pyarrow 的 SIMD 与
  Arrow/abseil 的 RANDEN 会探测并使用 AVX）；内核侧 CR4.OSXSAVE 与 XCR0 的开启在 dev 分支中。
- aarch64：`-cpu cortex-a72`（NEON 与 64 位特性集）。
- riscv64：`-cpu rv64`。
- loongarch64：`-machine virt -cpu la464`，动态平台（`build-loongarch64*.toml` 带
  `ax-driver/serial`，不含 `ax-hal/loongarch64-qemu-virt` / `plat-static` / `plat_dyn=false`）。
