# py-sci2 - Python 科学计算 + 机器学习地毯式测试（第二批）

py-sci 的姊妹套件，把 py-sci 暂缓的科学栈补全并一路推到机器学习。它由**两个独立配置、各自计入门控**的
栈组成，在 StarryOS 上真跑：

- **musl 栈**（Alpine `apk add py3-scipy py3-sympy`）：**SciPy + SymPy**，四个架构
  （x86_64 / aarch64 / riscv64 / loongarch64）全部覆盖。
- **glibc conda 栈**（Miniforge + conda-forge 预构建二进制）：**Numba（@njit MCJIT JIT）、pandas、
  scikit-learn、matplotlib、networkx、statsmodels**，覆盖 conda 官方发行的两个架构 **x86_64 / aarch64**。
  StarryOS 运行 glibc 用户态（gcompat 加载器 + 随附的 Debian libc6 闭包），因此一个可重定位的 Miniforge
  CPython 加上 conda-forge 预构建轮子，就能在 StarryOS 上跑起完整的 CPU 侧科学 + ML 栈。

断言全部采用与库补丁版本无关的精确不变量（闭式解析值容差比较 / 精确整数与结构比较 / 定点高精度前缀 /
分类器在种子固定数据集上的确定预测），因此宿主参照库与（更新的）目标端库给出逐字相同的结果，不依赖浮点
repr、默认 dtype 宽度或打印格式。

## 破墙：Numba 的 @njit JIT 在 StarryOS 上真编译真执行

py-sci 把 numba 记为墙 - musl 上无 `py3-numba` / `py3-llvmlite` 的 apk，也无 musllinux/riscv64/loongarch64
轮子，源码构建又对 LLVM 大版本极度敏感。本套件换一条路把它破了：**conda-forge 为 glibc 提供预构建的
numba 0.65.1 + llvmlite 0.47.0**，其中 `libllvmlite.so` 把 **LLVM 20 静态编进自身**（无需外部 `libLLVM`）。
StarryOS 通过随附的 Debian glibc 闭包运行这份 glibc Miniforge Python，`@njit` 经 llvmlite 把 Python 字节码
即时编译为原生机器码（LLVM MCJIT：`mmap` 可执行页 + 重定位），在目标机上**真编译、真执行**，结果与解释执行
逐字一致。numba 因此从"暂缓"变成**计入门控的地毯**。

## 地毯清单

| 模块 | 库 | 栈 | 覆盖维度 | 断言 | 标记 |
|:--|:--|:--|:--|:--:|:--|
| scipy | SciPy | musl | linalg（LU·Cholesky·SVD·QR·eigvalsh·expm·pinv·lstsq·norm）/ optimize（minimize·brentq·newton·fsolve·least_squares·curve_fit·linprog）/ integrate（quad·dblquad·simpson·solve_ivp）/ interpolate（interp1d·CubicSpline·splrep·Pchip·barycentric）/ fft（fft·rfft·dct·Parseval）/ signal（convolve·correlate·fftconvolve）/ sparse（csr·csc·coo·kron·spsolve）/ stats（norm·binom·poisson·linregress·spearmanr·ttest）/ special（gamma·erf·comb·beta·expit） | 76 | `SCIPY_DONE` |
| sympy | SymPy | musl | simplify·trigsimp / expand·factor·apart·cancel / solve（多项式·方程组·复数·非线性）/ diff·偏导·高阶 / integrate·limit·series / 有理数 / Matrix（det·inv·eigenvals·eigenvects·rref·nullspace·LU）/ 求和连乘闭式 / dsolve（一二阶 ODE）/ 数论（isprime·factorint·gcd·totient）/ 集合逻辑 / nsimplify·lambdify / 带重数根·Poly / 高精度 evalf | 68 | `SYMPY_DONE` |
| numba | Numba | conda | @njit 标量·数组归约·控制流·跨函数·递归·元组返回·numpy 内建 / 显式签名 / `prange` 并行归约 / `@vectorize` ufunc / `@guvectorize` / typed `List` / structured dtype / Mandelbrot 逃逸计数 / fastmath / MCJIT 稳态加速比 | 55 | `NUMBA_DONE` |
| pandas | pandas | conda | Series/DataFrame 构造·索引（loc/iloc/at/boolean）/ groupby·agg·transform / merge·join·concat / pivot·melt·stack / 时间序列（date_range·resample·rolling·shift）/ MultiIndex / apply·map / 缺失值 / 排序·rank / 分类 dtype / IO 往返（csv/json 内存缓冲） | 103 | `PANDAS_DONE` |
| scikit-learn | scikit-learn | conda | 全分类器（LogReg·SVC·RandomForest·GBDT·KNN·NaiveBayes·DecisionTree·MLP·LDA·QDA·Ridge·SGD）/ 回归器 / 聚类（KMeans·DBSCAN·Agglomerative）/ 降维（PCA·TruncatedSVD·NMF）/ 预处理（StandardScaler·MinMax·OneHot·Label·Poly）/ 管道·ColumnTransformer / 度量·交叉验证·GridSearch / 全数据集（iris·wine·digits·breast_cancer·make_classification）种子固定确定预测 | 133 | `SKLEARN_DONE` |
| matplotlib | matplotlib | conda | Agg 无头后端；line/scatter/bar/hist/pie/step/stem/errorbar/fill_between / imshow·pcolormesh·contour·contourf / 子图·GridSpec·twinx / 颜色映射·归一化 / 文本·注释·图例 / 变换·刻度定位器 / 渲染成 PNG 缓冲并断言尺寸/非空/像素不变量 | 79 | `MATPLOTLIB_DONE` |
| networkx | networkx | conda | 图构造（Graph·DiGraph·MultiGraph）/ 遍历（BFS·DFS）/ 最短路（dijkstra·bellman_ford·floyd_warshall·A*）/ 连通性·强连通 / 中心性（degree·betweenness·closeness·eigenvector·pagerank）/ MST（kruskal·prim）/ 匹配·流（max_flow·min_cut）/ 环·拓扑排序·着色 / 经典图生成器 | 74 | `NETWORKX_DONE` |
| statsmodels | statsmodels | conda | OLS·WLS·GLS / GLM（Binomial·Poisson·Gamma）/ Logit·Probit / ANOVA / 时间序列（AR·ARIMA·SARIMAX·acf·pacf）/ 描述统计·假设检验（t·适合度·Ljung-Box）/ 稳健回归 / 设计矩阵 - 全部对闭式或种子固定不变量 | 65 | `STATSMODELS_DONE` |
| conda-cli | conda | conda | conda 命令面地毯（见下方"conda 彩蛋"）| 102 | `CONDACLI_DONE` |

每个模块都是自包含地毯（独立 ok/fail 计数器，内部 fail 为 0 时才打印 `*_DONE` 标记）。
`run_pysci2.py` 先跑 musl 栈（scipy + sympy），再在 `/opt/miniconda/bin/python` 存在时跑 conda 栈（其余
七个）。仅当**当前架构上所有存在的地毯全部通过**时才打印 `PYSCI2_OK=<P>/<T>` 与 `TEST PASSED`
（不允许跳过）：conda 架构（x86_64 / aarch64）为 **9/9**，musl-only 架构（riscv64 / loongarch64）为 **2/2**。

## conda 彩蛋 - conda 命令面全 `--help` / `-h` 地毯

`CondaCliCarpet.py` 以 `conda --help` 自己的子命令树为 ground truth，逐条覆盖 conda 的整个命令面：
`clean / compare / config / create / info / init / install / list / notices / package / remove / rename /
run / search / update / env / export / doctor / repoquery / activate / deactivate` 每一个的 `--help` **与**
`-h` 都必须打印其 `usage: conda <sub>` 横幅并退出 0；再加上信息类命令返回真实、结构良好的输出
（`--version` 解析成 N.N.N、`info` / `info --json` 报版本与平台、`list` / `list numba` 列已装包、
`config --show` / `--show-sources` / `--describe`、`env list`、`run --help`、`doctor --help`）。共 102 条断言，
证明在 StarryOS 上跑起来的 conda 是一个功能完整、可自省的包管理器 - 而不仅仅是能 import 的 Python 库。

## glibc-on-StarryOS 机制

conda 栈的每个二进制都是 glibc 动态链接的 aarch64/x86_64 ELF，其 ELF 解释器烘焙为
`/lib64/ld-linux-x86-64.so.2`（x86）/ `/lib/ld-linux-aarch64.so.1`（aa）。`prebuild.sh` 的
`stage_conda_glibc` 从 Debian trixie 的 `libc6` 包取出真实加载器 + `libc.so.6` 及其同伴
（`libm`/`libpthread`/`libdl`/`librt`/…），按多架构路径注入 overlay，并把加载器复制到烘焙的解释器路径。

此外它注入 Debian `libc-bin` 里的 **static-pie `ldconfig`**（无 `NEEDED`/`INTERP`，可在 StarryOS 上裸跑）
以及一份 `ld.so.conf`；启动器 `run-pysci2.sh` 在跑地毯前先执行一次 `ldconfig` 生成 `/etc/ld.so.cache`。
这样 `ctypes.util.find_library("c")`（scikit-learn 依赖的 `threadpoolctl` 用它定位 libc 的
`dl_iterate_phdr` 来枚举 BLAS/OpenMP 线程池）会像一台正常 glibc 机器那样解析出 `libc.so.6`，而不是返回
`None` 再走一条在 StarryOS 上失败的 `dlopen(NULL)` 路径。

## 运行

```
cargo xtask starry app qemu -t py-sci2 --arch x86_64
cargo xtask starry app qemu -t py-sci2 --arch aarch64
cargo xtask starry app qemu -t py-sci2 --arch riscv64
cargo xtask starry app qemu -t py-sci2 --arch loongarch64
```

musl 栈：`prebuild.sh` 经 qemu-user-static 把 base Alpine rootfs 解到 staging 后指向 Alpine **v3.23**
（main + community），`apk add python3 py3-scipy py3-sympy` 由 apk 为目标架构解析当前版本及其完整 musl
原生 `.so` 闭包（scipy 带出 numpy / openblas / libgfortran / libquadmath，sympy 为 noarch 并带出 mpmath），
无写死漂移 URL、无缓存缺失即退出。

conda 栈（`PYSCI2_CONDA=1`，x86_64 / aarch64 默认开启）：x86_64 上直接跑 Miniforge 安装器 +
`conda install -c conda-forge`；aarch64 上在 x86_64 构建宿主用一次性宿主 conda 以
`CONDA_SUBDIR=linux-aarch64 conda create --platform linux-aarch64` **原生求解**（conda-forge 的 linux-aarch64
预构建二进制随之下载解压，无需 aarch64 模拟）。overlay 注入前把 per-app rootfs 镜像扩容到 8G
（truncate + e2fsck + resize2fs），以免 debugfs 注入多 GB conda 闭包时静默截断。

地毯源码位于 `python/`；启动器 `programs/run-pysci2.sh` 设置 musl 加载器搜索路径、`ldconfig` 生成
glibc 缓存、把 BLAS/OpenMP 线程固定为 1，然后执行 `run_pysci2.py`。

## 宿主验证

在 x86_64 原生 Alpine v3.23 chroot（`apk add python3 py3-scipy py3-sympy`）中解析到 python 3.12 /
scipy 1.16.3 / sympy 1.14.0，scipy 76/76、sympy 68/68。conda 栈在 conda-forge 环境（python 3.13 /
numpy 2.4 / numba 0.65.1 / scikit-learn）中，七个地毯逐一 fail=0。StarryOS 四架构运行验证在 qemu 阶段进行。

## 架构说明

- x86_64：`-cpu Haswell` 向用户态暴露 AVX2 + XSAVE（numpy / openblas / scipy 的 SIMD 内核探测并使用 AVX2；
  内核侧 CR4.OSXSAVE 与 XCR0 的开启在 dev 分支）；conda 栈需 `-m 4096M`。
- aarch64：`-cpu max` 暴露完整 AArch64 特性（LSE 原子 / dotprod / ARMv8.2+ NEON）；conda 栈需 `-m 4096M`。
- riscv64：`-cpu rv64`，musl scipy + sympy（conda 无 riscv64 发行）。
- loongarch64：`-machine virt -cpu la464`，动态平台（`build-loongarch64*.toml` 带 `ax-driver/serial`），
  musl scipy + sympy（conda 无 loongarch64 发行）。

conda 官方只发行 x86_64 / aarch64，riscv64 / loongarch64 无 conda 生态（上游缺架构，非内核问题），
这两个架构由 musl 的 scipy + sympy 覆盖。
