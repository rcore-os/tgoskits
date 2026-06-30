# go-lang — Go 1.26 语言级地毯式测试 (StarryOS app)

面向 StarryOS 的 **Go 1.26**（go1.26.3）语言级测试 app，覆盖 Go 语言层全貌：语言特性、并发/异步、标准库、Go 1.26 新特性，以及主流框架运行时（gin / grpc / go-zero / gorm + SQLite）。**2018 个断言**，逐项对照 go.dev/ref/spec + pkg.go.dev/std + go1.26 release notes + 各框架文档。

## 结构

- `go/` —— 测试源（`*.go` + `go.mod`/`go.sum`，`package main`，22 个分节文件）：
  - 语言：core/funcs/typesys/composite/range；并发：basics/sync/patterns；标准库：text/data/encoding/template-reflect/crypto-containers/misc；go1.26 features。
  - 框架：`framework_gin.go`（httptest）/`framework_grpc.go`（bufconn）/`framework_gozero.go`（rest httptest + zrpc bufconn + core 包）/`framework_gorm.go` + `framework_sqlite.go` + `framework_sqlite_comprehensive.go`（`modernc.org/sqlite` 纯 Go·CGO=0 驱动：虚表/DDL/事务/原子/权限）。
  - 聚合门控 `chk` 失败即 `os.Exit(1)`；全过打印 `GO_LANG_OK` + `GOLANG count=2018`。输出 100% 确定化（求和与顺序无关、map 经排序键、httptest/bufconn 内存驱动、时间用固定时刻、无地址/随机值泄漏），可逐字节比对。
- `prebuild.sh` —— 用官方 **go1.26.3** 工具链 `CGO_ENABLED=0 GOOS=linux GOARCH=<target>` 把 `go/` 交叉编译为**全静态二进制**（无 libc、无 interpreter），装到 overlay `/usr/local/bin/golang-lang`，并把 host golden 装到 `/root/golang-lang-golden.txt`。框架依赖按 `go.mod`/`go.sum` 固定版本拉取（`modernc.org/libc v1.73.4` 含 loong64 支持，故四架构均可纯 Go 静态编译）。
- `go/run-go.sh` —— on-target 门控脚本（作为 `shell_init_cmd` 整体调用，避免内联 echo 自匹配 `success_regex` 的假阳性）：跑 `golang-lang`，过滤 go-zero 在无 cgroup 内核下导入时打的非确定性 `@timestamp` JSON 诊断行，`grep GO_LANG_OK` 且 `cmp` host golden 通过才打印 `TEST PASSED`。
- `qemu-<arch>.toml` ×4 —— `success_regex = ^TEST PASSED$`，`fail_regex` 含 panic 与 `^TEST FAILED$`。
- `build-<target>.toml` ×4 + `golden.txt`。

## 运行

```sh
cargo xtask starry app qemu -t go-lang --arch x86_64    # aarch64 / riscv64 / loongarch64
```

## 覆盖

- **语言**：类型/转换、常量/iota、运算符、字符串/rune/byte、数组/切片/map（含 clear）、结构体+嵌入+tag、方法（值/指针）、接口+type set、类型断言/switch、泛型（约束/推断/泛型类型与方法/自引用类型参）、闭包、变参、defer/panic/recover、label、range（含 range-over-int 与 range-over-func 迭代器）、错误（Is/As/Join/wrap）。
- **并发**：goroutine、全 channel 操作、sync（Mutex/RWMutex/WaitGroup/Once/Map/Pool/Cond）、sync/atomic、context、worker-pool/fan-in-out/pipeline——全确定化。
- **标准库** ~40 包：fmt/strings/strconv/bytes/unicode/sort/slices/maps/cmp/math/math-big/math-rand-v2/math-bits/time/regexp/encoding-json|base64|hex|csv|binary/io/bufio/os/path/errors/context/reflect/hash-fnv|crc32/crypto-sha256|md5|hmac|sha3/container-list|heap|ring/text-template/html-template/compress-gzip/net-netip/log-slog/iter。
- **Go 1.26**：new(expr)、自引用泛型类型参数、errors.AsType[T]、bytes.Buffer.Peek、netip.Prefix.Compare、slog.NewMultiHandler、reflect 字段迭代、crypto/sha3。
- **框架**（运行时确定化，无真实端口/外部依赖）：gin v1.12.0（路由/中间件/RouterGroup/渲染/绑定，httptest）、google.golang.org/grpc v1.81.1（一元/流式/health/status/metadata/拦截器，bufconn）、go-zero v1.10.2（rest + zrpc + logx/mr/fx/mapping/stringx/syncx/collection 等 core 包）、gorm v1.31.1 + SQLite（`glebarez/sqlite` → `modernc.org/sqlite`：AutoMigrate/CRUD/Where/Preload/Association/Transaction/Hooks/Migrator/clause；虚表 FTS5、DDL 约束、CTE/窗口/UPSERT、SAVEPOINT 嵌套、FK 原子回滚、`mode=ro`/`PRAGMA query_only` 权限）。

> 说明：SQLite 用纯 Go `modernc.org/sqlite`（CGO=0），故能进全静态二进制并在 StarryOS 上真跑虚表/事务/外键，无需 libc。go-zero 在无 cgroup cpuset 的内核下导入时会打一条带 `@timestamp` 的 JSON 诊断（非断言输出），`run-go.sh` 已过滤以保持 golden 逐字节稳定。
