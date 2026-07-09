# Rust 代码质量约束准则

## 0. 总原则：代码首先是给人读的，其次才是给机器跑的

Rust 代码质量的核心目标是：

**让非法状态难以表示，让错误路径显式可见，让所有权关系清晰，让模块边界稳定，让抽象只承担一个责任。**

Rust 的整洁代码不应只是“写得漂亮”，而应同时满足：

| 维度       | Rust 版目标                              |
| -------- | ------------------------------------- |
| 可读性      | 名字表达意图，函数短小，模块边界清楚                    |
| 正确性      | 用类型系统、所有权、生命周期、`Result`/`Option` 表达约束 |
| 可维护性     | 低耦合、少重复、小 API、可测试                     |
| 可演进性     | 公共 API 少暴露实现细节，避免破坏性变更                |
| 可诊断性     | 错误、日志、`Debug`、测试信息足够定位问题              |
| Rust 惯用性 | 遵守 Rust 命名、格式化、trait、模块、错误处理习惯        |

Rust 官方书强调所有权是 Rust 的核心特性，它让 Rust 不依赖 GC 也能提供内存安全保证；因此 Rust 版整洁代码必须把“所有权是否清晰”当作一级质量指标。([Rust 文档][1])

## 0.1 报纸式阅读结构：先看标题，再读正文

源文件应像一份排版清楚的报纸：读者打开文件时，先看到标题和导语，马上知道这个文件解决什么问题；继续向下读时，看到一组按业务顺序排列的小节；最后才进入细节、边界处理和测试。

对应到 Rust 代码，推荐阅读层次如下：

| 报纸层次 | Rust 源文件层次 | 读者应获得的信息 |
|----------|----------------|------------------|
| 标题 | 模块说明、核心类型、整体功能入口 | 这个文件负责什么，主要能力从哪里开始读 |
| 导语 | 编排函数 | 完整流程有哪些步骤，步骤之间如何连接 |
| 正文 | 一个个命名清楚的步骤函数 | 每一步的业务规则、错误路径和状态变化 |
| 专栏 | 边界转换、辅助函数、底层细节 | 不影响主线阅读的局部实现 |
| 附录 | `#[cfg(test)] mod tests` | 行为约束和回归用例 |

一个可阅读的源文件应先给出整体功能函数，再让这个函数像目录一样列出内部步骤：

```rust
pub fn process_order(input: ProcessOrderInput) -> Result<OrderReceipt, OrderError> {
    let order = validate_order(input)?;
    let reservation = reserve_inventory(&order)?;
    let payment = charge_payment(&order, &reservation)?;
    let receipt = persist_receipt(order, reservation, payment)?;

    notify_customer(&receipt)?;
    Ok(receipt)
}

fn validate_order(input: ProcessOrderInput) -> Result<Order, OrderError> {
    // ...
}

fn reserve_inventory(order: &Order) -> Result<Reservation, OrderError> {
    // ...
}

fn charge_payment(
    order: &Order,
    reservation: &Reservation,
) -> Result<Payment, OrderError> {
    // ...
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_order_when_inventory_is_insufficient() {
        // ...
    }
}
```

这类结构让读者可以先读 `process_order` 建立全局理解，再按需要跳到某一步。不要把底层 helper、临时转换函数或测试模块放在主入口之前；那会迫使读者先读细节，再反推整体功能。

---

# 1. 命名准则

## 1.1 名字必须表达业务意图，而不是实现细节

**必须：**

```rust
let active_users = repository.find_active_users()?;
let retry_policy = RetryPolicy::exponential_backoff();
```

**禁止：**

```rust
let data = repo.get()?;
let flag = true;
let x = RetryPolicy::new(1, 3, true);
```

变量、函数、类型、模块名称必须让读者知道“它代表什么业务概念”或“它要完成什么动作”。

## 1.2 遵守 Rust 命名惯例

Rust API Guidelines 规定，类型和 trait 通常使用 `UpperCamelCase`，函数、方法、变量、模块使用 `snake_case`，常量使用 `SCREAMING_SNAKE_CASE`。([Rust语言][2])

| 对象                    | 规则                     | 示例                                           |
| --------------------- | ---------------------- | -------------------------------------------- |
| struct / enum / trait | `UpperCamelCase`       | `UserProfile`, `PaymentStatus`, `Repository` |
| 函数 / 方法               | `snake_case`           | `find_user`, `validate_token`                |
| 模块                    | `snake_case`           | `user_service`, `payment_gateway`            |
| 常量                    | `SCREAMING_SNAKE_CASE` | `MAX_RETRY_COUNT`                            |
| 生命周期                  | 短小但有意义                 | `'a`, `'de`, `'src`                          |
| 泛型                    | 简洁                     | `T`, `E`, `S`, `R`                           |

## 1.3 布尔参数必须替换为有意义的类型

**禁止：**

```rust
create_user(name, true, false);
```

**必须改为：**

```rust
create_user(
    name,
    EmailVerification::Required,
    WelcomeEmail::Disabled,
);
```

Rust API Guidelines 明确建议：参数含义应由类型表达，而不是用 `bool`、裸 `u8` 或含义模糊的 `Option` 承载。([Rust语言][3])

---

# 2. 函数准则

## 2.1 函数只做一件事，并且函数名能概括这件事

一个函数如果需要用“并且”“然后”“同时”描述，通常应拆分。

**禁止：**

```rust
fn process_order(order: Order) -> Result<(), Error> {
    validate_order(&order)?;
    reserve_inventory(&order)?;
    charge_payment(&order)?;
    send_email(&order)?;
    Ok(())
}
```

如果这是编排函数，可以保留；但每一步必须是清楚的独立函数。业务细节不能全部堆在一个函数中。

**推荐：**

```rust
fn process_order(order: Order) -> Result<(), OrderError> {
    validate_order(&order)?;
    let reservation = reserve_inventory(&order)?;
    charge_payment(&order, &reservation)?;
    notify_customer(&order)?;
    Ok(())
}
```

## 2.2 整体功能函数应放在源文件开头

一个源文件如果承载一个主要功能，读者应该能在文件开头看到这个功能的入口。入口函数不一定必须是 `pub`，但它必须表达该文件的主线能力。

**禁止：**

```rust
fn normalize_price(value: i64) -> Money {
    // ...
}

fn parse_currency(input: &str) -> Result<Currency, Error> {
    // ...
}

#[cfg(test)]
mod tests {
    // ...
}

pub fn checkout(input: CheckoutInput) -> Result<Receipt, CheckoutError> {
    // main flow hidden after details
}
```

**推荐：**

```rust
pub fn checkout(input: CheckoutInput) -> Result<Receipt, CheckoutError> {
    let request = validate_checkout_input(input)?;
    let priced_cart = price_cart(&request)?;
    let payment = charge_customer(&request, &priced_cart)?;
    let receipt = create_receipt(request, priced_cart, payment)?;

    Ok(receipt)
}

fn validate_checkout_input(input: CheckoutInput) -> Result<CheckoutRequest, CheckoutError> {
    // ...
}

fn price_cart(request: &CheckoutRequest) -> Result<PricedCart, CheckoutError> {
    // ...
}
```

如果文件包含多个同级入口，应先列出最重要、最常读的入口，再列出次要入口。不要让读者在 helper、常量、测试和底层适配之间寻找“这个文件到底从哪里开始”。

## 2.3 编排函数只表达流程目录，不夹杂底层细节

编排函数的职责是让读者看懂流程，而不是在一屏代码里完成所有工作。它应该像目录：每一行调用一个表达业务动作的函数，调用顺序就是阅读顺序。

**禁止：**

```rust
pub fn boot_system(fdt: *const u8) -> Result<(), BootError> {
    if fdt.is_null() {
        return Err(BootError::MissingFdt);
    }

    let header = unsafe { read_fdt_header(fdt) };
    if header.magic != FDT_MAGIC {
        return Err(BootError::InvalidFdt);
    }

    for node in unsafe { iter_fdt_nodes(fdt) } {
        if node.name == "memory" {
            // parse ranges, align pages, merge regions...
        }
    }

    // initialize IRQ, timer, console, scheduler...
    Ok(())
}
```

**推荐：**

```rust
pub fn boot_system(fdt: NonNull<u8>) -> Result<(), BootError> {
    let firmware = read_firmware_tables(fdt)?;
    let memory = build_memory_layout(&firmware)?;
    let devices = discover_boot_devices(&firmware)?;

    initialize_console(&devices)?;
    initialize_interrupts(&devices)?;
    initialize_timer(&devices)?;
    start_scheduler(memory)?;

    Ok(())
}
```

底层细节仍然存在，但被移动到步骤函数中。读者第一遍只需要理解“启动分几步”，第二遍才进入某一步的具体解析。

## 2.4 函数应按阅读顺序向下展开

同一个源文件内，函数排序应尽量遵循“从主线到细节”的方向：

1. 公共入口或该文件的核心入口函数。
2. 入口函数直接调用的主要编排步骤。
3. 每个步骤内部使用的领域规则函数。
4. 边界转换、格式化、错误映射、底层 helper。
5. `#[cfg(test)] mod tests`，并放在文件最后。

如果一个 helper 只服务于某个步骤，优先放在该步骤函数之后，而不是统一堆到文件顶部或底部。这样读者沿着入口向下读，就能像阅读目录和正文一样逐层展开。

例外情况需要有明确理由：

- 宏生成或 Rust item 顺序限制要求提前定义。
- 常量、类型别名或 trait bound 会影响入口函数签名，需要放在入口前帮助理解。
- 多个入口共享同一组重要类型，应先定义这些类型，再给出入口函数。

## 2.5 函数参数不应过多

超过 3 个参数时，优先考虑：

1. 引入配置 struct；
2. 引入 builder；
3. 引入领域对象；
4. 拆分函数职责。

**禁止：**

```rust
fn connect(
    host: String,
    port: u16,
    timeout_ms: u64,
    use_tls: bool,
    retry_count: u8,
) -> Result<Client, Error>
```

**推荐：**

```rust
let client = ClientConfig::new(host, port)
    .timeout(Duration::from_secs(3))
    .tls(TlsMode::Required)
    .retries(3)
    .connect()?;
```

Rust API Guidelines 也建议复杂对象使用 builder，尤其是参数多、可选项多、有副作用或有多种构造方式的类型。([Rust语言][3])

## 2.6 不使用输出参数

**禁止：**

```rust
fn parse_user(input: &str, output: &mut User) -> Result<(), ParseError>
```

**推荐：**

```rust
fn parse_user(input: &str) -> Result<User, ParseError>
```

Rust 中返回值、元组、struct、`Result<T, E>` 已足够表达结果。输出参数会降低可读性，也让所有权更难判断。

## 2.7 优先让调用方控制分配

公共 API 不应强迫调用方接受不必要的 `String`、`Vec`、clone 或堆分配。

**优先：**

```rust
fn find_user(id: UserId) -> Result<User, Error>;

fn parse(input: &str) -> Result<Command, ParseError>;

fn write_report<W: Write>(writer: W, report: &Report) -> io::Result<()>;
```

**谨慎：**

```rust
fn parse(input: String) -> Result<Command, ParseError>;
```

除非函数确实需要取得所有权，否则优先接收借用。

---

# 3. 注释与文档准则

## 3.1 注释解释“为什么”，不是复述“做了什么”

**禁止：**

```rust
// increment i by 1
i += 1;
```

**推荐：**

```rust
// The upstream API uses 1-based page numbers.
let external_page = internal_page + 1;
```

代码能表达“做了什么”；注释应解释背景、约束、边界条件、权衡和历史原因。

## 3.2 公共 API 必须有 rustdoc

公共的 `struct`、`enum`、`trait`、函数、模块必须说明：

* 它解决什么问题；
* 如何使用；
* 什么时候返回错误；
* 什么时候 panic；
* 如果有 `unsafe`，调用方必须满足什么安全条件。

Rust API Guidelines 建议公共项提供 rustdoc 示例，错误条件放在 `# Errors`，panic 条件放在 `# Panics`，unsafe 函数放在 `# Safety`。([Rust语言][4])

**示例：**

```rust
/// Loads a user by id.
///
/// # Errors
///
/// Returns [`UserError::NotFound`] if the user does not exist.
/// Returns [`UserError::Storage`] if the repository cannot be accessed.
pub fn load_user(id: UserId) -> Result<User, UserError> {
    // ...
}
```

## 3.3 文档示例不能滥用 `unwrap`

文档示例经常会被用户复制。Rust API Guidelines 建议示例使用 `?`，而不是 `unwrap`。([Rust语言][4])

**推荐：**

````rust
/// ```rust
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let config = Config::from_file("app.toml")?;
/// # Ok(())
/// # }
/// ```
````

---

# 4. 格式化与风格准则

## 4.1 格式统一交给 rustfmt

所有项目必须使用：

```bash
cargo fmt --all --check
```

Rust Style Guide 说明，统一格式能减少沟通成本和认知负担，`rustfmt` 使用 Rust Style Guide 作为默认风格参考。([Rust 文档][5])

推荐团队不要为缩进、换行、大括号风格争论；这些交给工具。

## 4.2 使用 Clippy 检查常见错误和复杂写法

---

# 5. 错误处理准则

## 5.1 可恢复错误必须用 `Result`

**禁止：**

```rust
fn load_config() -> Config {
    std::fs::read_to_string("config.toml").unwrap();
    // ...
}
```

**推荐：**

```rust
fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let content = std::fs::read_to_string(path)?;
    Config::parse(&content)
}
```

Rust 官方书说明，`unwrap` 在 `Err` 时会 panic；生产代码中即使确实认为不会失败，也更推荐使用带上下文的 `expect`，以便假设被破坏时更容易诊断。([Rust 文档][8])

## 5.2 `unwrap` 只能出现在受控位置

允许：

```rust
#[test]
fn parses_valid_user_id() {
    let id = UserId::parse("u_123").unwrap();
    assert_eq!(id.as_str(), "u_123");
}
```

谨慎允许：

```rust
let regex = Regex::new(r"^\d+$").expect("hard-coded regex must be valid");
```

禁止：

```rust
let user = repository.find_user(id).unwrap();
```

生产路径中的失败必须显式传播、转换或处理。

## 5.3 库用具体错误类型，应用可用聚合错误

库 crate 的公共 API 应返回具体错误类型：

```rust
pub fn parse_user(input: &str) -> Result<User, UserParseError>
```

应用层可以使用聚合错误：

```rust
fn main() -> anyhow::Result<()> {
    run()
}
```

公共错误类型应实现 `std::error::Error`、`Display`、`Debug`，并尽可能满足 `Send + Sync + 'static`。Rust API Guidelines 明确建议公共 `Result<T, E>` 的错误类型要有意义、可表现良好，并且不要使用 `()` 作为错误类型。([Rust语言][9])

## 5.4 `panic!` 只用于程序员错误或不可恢复不变量破坏

允许：

```rust
assert!(capacity > 0, "capacity must be positive");
```

禁止把业务失败写成 panic：

```rust
panic!("user not found");
```

应改为：

```rust
return Err(UserError::NotFound(id));
```

---

# 6. 类型系统准则

## 6.1 用类型表达约束，不靠注释和约定

**禁止：**

```rust
fn transfer(amount: i64, currency: String)
```

**推荐：**

```rust
fn transfer(amount: Money)
```

```rust
pub struct Money {
    cents: NonZeroI64,
    currency: Currency,
}
```

Rust API Guidelines 推荐用 newtype 区分同一底层类型的不同语义，例如英里和公里，避免把含义不同的值混用。([Rust语言][3])

## 6.2 非法状态应尽量无法构造

**禁止：**

```rust
pub struct User {
    pub email: String,
    pub age: i32,
}
```

**推荐：**

```rust
pub struct User {
    email: Email,
    age: Age,
}

impl User {
    pub fn new(email: Email, age: Age) -> Self {
        Self { email, age }
    }
}
```

公共字段会把内部表示暴露给调用方，使类型无法维护不变量。Rust API Guidelines 建议除被动数据结构外，struct 字段应保持私有，通过构造函数和方法维护约束。([Rust语言][10])

## 6.3 使用 enum 表达封闭集合

当所有变体在当前领域内已知时，优先使用 enum，而不是 trait object。

```rust
enum PaymentMethod {
    Card(CardPayment),
    BankTransfer(BankTransfer),
    Wallet(WalletPayment),
}
```

适合 enum 的情况：

* 状态集合固定；
* 调用方需要 exhaustive match；
* 每个变体数据不同；
* 不希望外部扩展新类型。

适合 trait 的情况：

* 实现类型开放；
* 插件式扩展；
* 调用方可定义自己的类型；
* 只依赖共同能力而非具体数据。

---

# 7. 模块与边界准则

## 7.1 模块不是文件夹，而是边界

Rust 模块应围绕领域边界组织，而不是机械地按技术层拆散。

**推荐：**

```text
src/
  user/
    mod.rs
    entity.rs
    repository.rs
    service.rs
    error.rs
  payment/
    mod.rs
    method.rs
    gateway.rs
    error.rs
```

模块内聚目标：

* 同一模块内的类型经常一起变化；
* 模块对外暴露少量稳定接口；
* 模块内部细节默认私有；
* 跨模块调用通过明确 API，而不是到处 `pub`。

Rust 的可见性机制支持 `pub(crate)`、`pub(super)`、`pub(in path)` 等范围限制，适合表达“只在 crate 内可见”或“只给父模块使用”的边界。([Rust 文档][11])

## 7.2 默认私有，按需公开

**禁止：**

```rust
pub struct UserService {
    pub repository: UserRepository,
    pub cache: Cache,
}
```

**推荐：**

```rust
pub struct UserService {
    repository: UserRepository,
    cache: Cache,
}
```

```rust
impl UserService {
    pub fn new(repository: UserRepository, cache: Cache) -> Self {
        Self { repository, cache }
    }
}
```

## 7.3 第三方依赖必须隔离在边界层

不要让外部库类型污染整个业务层。

**禁止：**

```rust
pub fn create_user(row: sqlx::postgres::PgRow) -> Result<User, sqlx::Error>
```

**推荐：**

```rust
pub trait UserRepository {
    fn find_by_id(&self, id: UserId) -> Result<Option<User>, UserRepositoryError>;
}
```

基础设施层负责把 `sqlx::Error` 转成领域错误。

## 7.4 单文件内也要有阅读顺序

模块边界解决“哪些代码应该放在一起”，单文件顺序解决“读者应该怎样进入这些代码”。同一个 Rust 源文件推荐按以下顺序组织：

| 顺序 | 内容 | 目的 |
|------|------|------|
| 1 | 模块级 rustdoc、必要 `use`、核心类型和错误类型 | 建立上下文 |
| 2 | 该文件最重要的入口函数或公共 API | 先看到整体功能 |
| 3 | 入口函数直接调用的步骤函数 | 按目录顺序展开主线 |
| 4 | 步骤内部 helper、转换函数、错误映射和底层细节 | 支撑局部实现 |
| 5 | `#[cfg(test)] mod tests` | 把验证作为附录放在文件最后 |

**禁止：**

```rust
fn parse_flags(raw: u64) -> Flags {
    // low-level helper
}

fn align_down(addr: usize) -> usize {
    // low-level helper
}

pub fn map_user_region(request: MapRequest) -> Result<Mapping, MapError> {
    // main entry appears too late
}
```

**推荐：**

```rust
pub fn map_user_region(request: MapRequest) -> Result<Mapping, MapError> {
    let request = validate_map_request(request)?;
    let pages = allocate_user_pages(&request)?;
    let mapping = install_page_table_entries(&request, pages)?;

    Ok(mapping)
}

fn validate_map_request(request: MapRequest) -> Result<ValidatedMapRequest, MapError> {
    // ...
}

fn allocate_user_pages(
    request: &ValidatedMapRequest,
) -> Result<PageAllocation, MapError> {
    // ...
}
```

底层 helper 不是不能存在，而是不应该抢在主线之前出现。读者应先知道“这个模块做什么”，再读“它如何做到”。

## 7.5 `lib.rs`、`mod.rs` 和领域模块的组织方式

`lib.rs` 和 `mod.rs` 是读者进入 crate 或领域模块的目录页，应避免变成无序的 re-export 仓库。

推荐规则：

- `lib.rs` 先写 crate 级说明，再声明内部模块，最后 re-export 稳定入口。
- `mod.rs` 先说明该领域模块的职责，再列出子模块和对外 API。
- 领域模块文件先给核心类型和入口函数，再展开领域步骤。
- `utils`、`helpers`、`common` 这类泛名模块只在确实跨领域共享且职责清楚时使用。
- 测试模块不应插在生产代码中间；单元测试放源文件最后，集成测试放 `tests/`。

示例结构：

```text
src/
  lib.rs                 # crate 说明、模块声明、稳定 re-export
  process/
    mod.rs               # process 领域入口和子模块目录
    lifecycle.rs         # 入口函数在前，状态转换步骤向下展开
    signal.rs            # signal 规则和边界
    error.rs             # process 领域错误
```

如果 `mod.rs` 已经长到需要滚动多屏才能看完入口，说明它承担了太多实现细节，应把具体逻辑下沉到领域文件中。

## 7.6 源文件过大必须拆分

Rust 源文件不是越集中越好。一个文件承担太多职责时，读者需要同时记住入口、状态、错误、边界适配、helper、测试和外部依赖，报纸式阅读结构会失效。

推荐把文件大小作为 code review 的触发线，而不是机械的 CI 硬限制：

| 信号 | 处理方式 |
|------|----------|
| 约 400 行以上 | 审视是否混入多种变化原因，优先拆出错误、状态、适配或测试 |
| 约 800 行以上 | 必须拆分，或在评审中说明为什么当前文件仍是单一职责 |
| 一个文件同时有入口、状态机、外部 IO、错误转换、测试 helper | 按职责拆文件 |
| `mod.rs` / `lib.rs` 承载大量业务实现 | 下沉到领域文件，入口页只保留模块声明和稳定导出 |

拆分时不要按“代码行数平均分”切文件，而要按变化原因切。Robert C. Martin 对 Single Responsibility Principle（SRP）的解释是：一个模块应只对一类 actor 或一类变化原因负责。落到 Rust 中，就是同因变化的类型和函数放在一起，异因变化的实现拆到不同文件。([Clean Coder][24])

常见拆分方向：

| 变化原因 | 推荐文件 |
|----------|----------|
| 对外入口和主流程变化 | `mod.rs` 或 `service.rs` |
| 领域状态和状态转换变化 | `state.rs`、`lifecycle.rs` |
| 错误语义变化 | `error.rs` |
| 配置解析和默认值变化 | `config.rs` |
| 外部存储、网络、硬件、系统调用适配变化 | `repository.rs`、`adapter.rs`、`runtime.rs` |
| 测试 fixture、mock、断言变化 | `tests/` 或源文件末尾的 `mod tests` |

## 7.7 标准目录结构遵循 Cargo 和 Rust 模块约定

Cargo 对 package 的常见布局已有约定：`src/lib.rs` 是库入口，`src/main.rs` 是默认二进制入口，`src/bin/*.rs` 是额外二进制，`tests/` 是集成测试，`examples/` 和 `benches/` 分别放示例和 benchmark。([Cargo 文档][22])

推荐目录结构：

```text
my_crate/
  Cargo.toml
  src/
    lib.rs                 # crate 说明、模块声明、稳定 re-export
    main.rs                # 二进制入口，仅做启动编排
    bin/
      admin.rs             # 额外二进制
    process/
      mod.rs               # process 领域目录页
      lifecycle.rs         # 生命周期主流程
      state.rs             # 状态和状态转换
      error.rs             # 领域错误
      repository.rs        # 存储边界 trait 或 adapter
  tests/
    process_lifecycle.rs   # 集成测试
    process/
      common.rs            # 多文件集成测试共享 helper
```

Rust 支持把模块拆到同名文件或同名目录下的 `mod.rs` 中，但同一个模块不能同时使用 `foo.rs` 和 `foo/mod.rs` 两种文件。Rust Book 也建议模块增长时拆到独立文件，保持 `mod` 声明处的模块树不变。([Rust 文档][23])

选择规则：

- 新模块优先使用 `foo.rs` + `foo/child.rs` 风格，减少多级目录中大量 `mod.rs` 带来的定位成本。
- 已有模块使用 `mod.rs` 时，保持局部一致，不为一次文档或小重构混用风格。
- `main.rs` 只做参数解析、初始化、错误报告和调用库入口，不承载业务规则。
- `lib.rs` 只暴露稳定 crate 边界，不把内部目录结构原样泄漏给调用方。

## 7.8 re-export 可以用 `*` 简化，但必须守住边界

re-export 的目标是提供稳定入口，而不是把内部模块全部摊开。大量逐个手写 re-export 会制造维护噪音：内部新增或重命名一个稳定类型时，需要同步改很多行。对边界清晰、意图稳定的领域出口，可以用 `pub use module::*;` 聚合。

**推荐：**

```rust
mod lifecycle;
mod state;
mod error;

pub use error::*;
pub use lifecycle::*;
pub use state::*;
```

这种写法适合 `error.rs`、`types.rs`、`prelude.rs`、领域公共入口这类已经被设计成稳定出口的模块。

**谨慎：**

```rust
mod parser;
mod raw;
mod unsafe_impl;

pub use parser::*;
pub use raw::*;
pub use unsafe_impl::*;
```

如果模块内部包含临时 helper、raw descriptor、unsafe 封装细节或平台私有实现，不应直接 `pub use *`。先把稳定公共项整理到清晰的出口模块，再用 `*` 聚合。

判断规则：

- `pub use module::*;` 只能用于“这个模块里的公共项本来就都应该成为上层 API”的情况。
- 如果需要排除很多项，说明模块边界不适合 `*`，应拆出 `api.rs`、`prelude.rs` 或更小的领域模块。
- 不要用 re-export 掩盖糟糕目录结构。先拆职责，再决定如何导出。
- crate 对外 API 可以简洁，但内部模块仍应保持私有优先。

## 7.9 拆分模块的步骤

面对一个大文件时，按以下顺序拆：

1. 找入口：确定读者第一眼应该看到的主流程函数。
2. 找变化原因：把会被不同需求修改的代码用标记分组，例如配置、状态、错误、IO、策略、测试。
3. 找所有权边界：把持有资源、锁、缓存、句柄的类型单独成文件。
4. 找外部边界：把数据库、网络、硬件、syscall、文件系统、时间等外部依赖移到 adapter 或 repository。
5. 找稳定出口：让 `mod.rs` / `lib.rs` 只导出领域入口、错误、核心类型和必要 trait。
6. 保留阅读顺序：每个拆出的文件仍保持入口在前、步骤向下展开、测试在最后。

拆分后的模块应该能回答三个问题：这个文件为什么会变，它对外提供什么能力，它依赖哪些边界。

---

# 8. 继承替代准则：Rust 风格组合设计

Rust 不提供传统面向对象继承。Rust 官方书明确指出，Rust 使用 trait object 代替继承来实现运行期多态；同时 trait object 只抽象共同行为，不像传统对象那样把数据和行为合并成一个“类”。([Rust 文档][12])

因此，原书中关于“类、继承、基类、派生类”的规则，在 Rust 中应改写为：

> **不要问“这个类型继承自谁”，而要问：它拥有什么数据？实现什么行为？组合了哪些能力？暴露了什么边界？**

---

## 8.1 继承场景到 Rust 方案映射

| 传统 OOP 需求     | Rust 方案                             |
| ------------- | ----------------------------------- |
| 多个类型共享行为      | `trait`                             |
| 多个类型共享默认行为    | trait 默认方法 + 小 trait                |
| 多个类型共享状态      | 组合 struct 字段                        |
| 子类复用父类字段      | 把公共字段抽成组件 struct                    |
| 多态调用          | 泛型 `T: Trait` 或 `dyn Trait`         |
| 运行时存放不同实现     | `Box<dyn Trait>` / `Arc<dyn Trait>` |
| 封闭状态机         | `enum` 或 typestate                  |
| 开放插件扩展        | public trait                        |
| 不希望外部实现 trait | sealed trait                        |
| 多继承           | 多 trait bound + 多字段组合               |
| 覆盖父类方法        | trait 实现或策略对象                       |
| 模板方法模式        | 函数编排 + trait hook，谨慎使用              |
| 基类工具方法        | 独立函数、扩展 trait、组合组件                  |

---

## 8.2 共享行为：使用 trait

```rust
pub trait Draw {
    fn draw(&self);
}

pub struct Button {
    label: String,
}

impl Draw for Button {
    fn draw(&self) {
        // draw button
    }
}
```

Rust 官方书说明，trait 用来定义多个类型可共享的行为，并可通过 trait bound 限定泛型类型必须具备某种能力。([Rust 文档][13])

---

## 8.3 共享状态：使用组合，不要试图把状态塞进 trait

传统继承：

```text
Animal
  - name
  - age

Dog extends Animal
Cat extends Animal
```

Rust 版：

```rust
pub struct AnimalCore {
    name: String,
    age: u8,
}

impl AnimalCore {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn age(&self) -> u8 {
        self.age
    }
}

pub struct Dog {
    core: AnimalCore,
    breed: String,
}

pub struct Cat {
    core: AnimalCore,
    lives: u8,
}
```

通过委托暴露需要的行为：

```rust
impl Dog {
    pub fn name(&self) -> &str {
        self.core.name()
    }

    pub fn age(&self) -> u8 {
        self.core.age()
    }
}
```

trait 表达能力，struct 表达数据。不要为了模拟继承，把所有字段都做成 public，也不要滥用 `Deref` 让 `Dog` “看起来像” `AnimalCore`。Rust API Guidelines 建议只有智能指针才应实现 `Deref`/`DerefMut`，避免制造出令人意外的方法解析和 API 行为。([Rust语言][14])

---

## 8.4 运行期多态：使用 `dyn Trait`

当你需要把多个不同具体类型放在同一个集合里：

```rust
pub trait NotificationChannel {
    fn send(&self, message: &Message) -> Result<(), SendError>;
}

pub struct Notifier {
    channels: Vec<Box<dyn NotificationChannel>>,
}

impl Notifier {
    pub fn notify_all(&self, message: &Message) -> Result<(), SendError> {
        for channel in &self.channels {
            channel.send(message)?;
        }
        Ok(())
    }
}
```

适合 `dyn Trait` 的情况：

* 实现类型在运行期才确定；
* 需要异构集合；
* 插件式扩展；
* 性能瓶颈不在动态分发；
* API 需要隐藏具体实现类型。

---

## 8.5 编译期多态：优先使用泛型

如果调用方不需要异构集合，优先用泛型：

```rust
pub fn render<T: Draw>(component: &T) {
    component.draw();
}
```

或：

```rust
pub struct Renderer<T: Draw> {
    component: T,
}
```

泛型适合：

* 性能敏感；
* 类型在编译期确定；
* 不需要把不同类型放进同一个容器；
* 需要静态分发和内联优化。

---

## 8.6 封闭状态机：优先 enum，而不是 trait object

传统状态模式经常通过继承实现。Rust 中如果状态集合固定，优先 enum：

```rust
enum OrderState {
    Created,
    Paid,
    Shipped,
    Cancelled,
}

impl OrderState {
    fn can_cancel(&self) -> bool {
        matches!(self, Self::Created | Self::Paid)
    }
}
```

如果每个状态携带不同数据：

```rust
enum Order {
    Created(CreatedOrder),
    Paid(PaidOrder),
    Shipped(ShippedOrder),
    Cancelled(CancelledOrder),
}
```

如果需要开放扩展，再考虑 trait object。Rust 官方书在讨论状态模式时也强调，在 Rust 中使用 struct 和 trait，而不是对象和继承。([Rust 文档][15])

---

## 8.7 策略模式：使用 trait 或函数

```rust
pub trait PricingStrategy {
    fn calculate(&self, cart: &Cart) -> Money;
}

pub struct Checkout<S: PricingStrategy> {
    pricing: S,
}

impl<S: PricingStrategy> Checkout<S> {
    pub fn total(&self, cart: &Cart) -> Money {
        self.pricing.calculate(cart)
    }
}
```

如果策略只是一个简单函数：

```rust
pub struct Checkout<F>
where
    F: Fn(&Cart) -> Money,
{
    pricing: F,
}
```

不要为了“设计模式完整”而引入不必要的 trait。能用函数表达的策略，不必强行对象化。

---

## 8.8 trait 必须小而专注

**禁止：**

```rust
pub trait UserService {
    fn create_user(&self, input: CreateUser) -> Result<User, Error>;
    fn delete_user(&self, id: UserId) -> Result<(), Error>;
    fn send_email(&self, user: &User) -> Result<(), Error>;
    fn export_csv(&self) -> Result<String, Error>;
    fn refresh_cache(&self) -> Result<(), Error>;
}
```

**推荐拆分：**

```rust
pub trait CreateUser {
    fn create_user(&self, input: CreateUserInput) -> Result<User, CreateUserError>;
}

pub trait DeleteUser {
    fn delete_user(&self, id: UserId) -> Result<(), DeleteUserError>;
}

pub trait NotifyUser {
    fn notify_user(&self, user: &User, message: Message) -> Result<(), NotifyError>;
}
```

trait 应按“能力”命名，而不是按“巨型服务对象”命名。

---

## 8.9 不希望外部实现的 trait 应 sealed

如果某个 trait 只允许当前 crate 内部类型实现，使用 sealed trait：

```rust
pub trait Event: private::Sealed {
    fn event_type(&self) -> &'static str;
}

mod private {
    pub trait Sealed {}
}
```

Rust API Guidelines 推荐 sealed trait 用于保护未来演进空间：如果 trait 不允许下游 crate 实现，后续添加方法时更不容易造成破坏性变更。([Rust语言][10])

## 8.10 复用优先级：先组合，再抽象

Rust 中的复用不应模仿继承层级。推荐优先级如下：

| 复用目标 | Rust 方式 | 适用场景 |
|----------|-----------|----------|
| 复用状态 | 组合 struct 字段 | 多个对象共享同一组数据和不变量 |
| 复用行为 | 小 trait | 多个类型提供同一种能力 |
| 复用静态能力 | 泛型 `T: Trait` | 调用方类型在编译期确定，性能敏感 |
| 复用运行时开放扩展 | `dyn Trait` | 插件、异构集合、运行时选择实现 |
| 复用底层表示 | newtype | 同一底层类型有不同语义 |
| 复用出口 | `pub use module::*;` | 聚合稳定公共入口，减少机械 re-export |

不要一开始就引入“大 trait + 大 struct + dyn Trait”。先用具体类型把职责切清楚，再在真实重复出现时抽出 trait 或泛型。

## 8.11 共享代码不等于共享状态

为了复用而制造共享大对象，是 Rust 项目里常见的继承式坏味道。

**禁止：**

```rust
pub struct AppContext {
    config: Config,
    user_repository: UserRepository,
    payment_gateway: PaymentGateway,
    email_client: EmailClient,
    metrics: Metrics,
    cache: Cache,
}

impl AppContext {
    pub fn create_user(&self, input: CreateUserInput) -> Result<User, Error> {
        // uses repository, email, metrics, cache...
    }

    pub fn checkout(&self, input: CheckoutInput) -> Result<Receipt, Error> {
        // uses payment, repository, metrics, cache...
    }
}
```

这个对象看似方便复用，实际把用户、支付、邮件、指标、缓存的变化原因绑定在一起。任何一个能力变化都会迫使 `AppContext` 变大。

**推荐：**

```rust
pub struct CreateUser<R, N> {
    repository: R,
    notifier: N,
}

impl<R, N> CreateUser<R, N>
where
    R: UserRepository,
    N: UserNotifier,
{
    pub fn execute(&self, input: CreateUserInput) -> Result<User, CreateUserError> {
        let user = User::register(input)?;
        self.repository.save(&user)?;
        self.notifier.welcome(&user)?;
        Ok(user)
    }
}
```

复用点变成两个小能力：`UserRepository` 和 `UserNotifier`。创建用户不再依赖支付、缓存或全局上下文。

## 8.12 re-export 只能复用入口，不能复用职责

`pub use module::*;` 可以减少机械导出，但它不是架构边界。真正的边界仍来自模块职责、类型可见性和 trait 设计。

推荐模式：

```rust
pub mod prelude {
    pub use crate::error::*;
    pub use crate::lifecycle::*;
    pub use crate::state::*;
}
```

调用方可以通过 `prelude` 获得稳定入口；内部实现仍保持私有。不要把 `raw`、`imp`、`sys`、`unsafe_impl` 这类模块整体 re-export 给外部。

---

# 9. 对象、数据结构与领域模型准则

## 9.1 行为靠方法，数据靠类型，不要贫血也不要上帝对象

贫血模型：

```rust
pub struct User {
    pub email: String,
    pub status: String,
}
```

业务规则散落在外部函数中。

推荐：

```rust
pub struct User {
    email: Email,
    status: UserStatus,
}

impl User {
    pub fn activate(&mut self) -> Result<(), UserError> {
        if self.status == UserStatus::Banned {
            return Err(UserError::BannedUserCannotBeActivated);
        }

        self.status = UserStatus::Active;
        Ok(())
    }
}
```

领域对象应维护自己的不变量。

## 9.2 DTO 与领域对象必须区分

```rust
#[derive(Deserialize)]
pub struct CreateUserRequest {
    email: String,
    password: String,
}
```

```rust
pub struct User {
    email: Email,
    password_hash: PasswordHash,
}
```

请求对象、数据库行对象、领域对象不要混用。边界层负责转换和校验。

## 9.3 结构体过大必须拆分

结构体是 Rust 中承载不变量和所有权的核心单元。结构体过大时，问题通常不是字段数量本身，而是多个变化原因、生命周期和并发上下文被塞进同一个对象。

评审触发线：

| 信号 | 说明 |
|------|------|
| 字段约 8 个以上 | 检查是否混入配置、状态、资源、策略、缓存和统计 |
| 同时持有多个锁或原子状态 | 检查是否存在不同并发上下文 |
| 同时持有外部 IO client 和领域状态 | 检查边界适配是否污染领域对象 |
| `impl` 中方法跨越多个业务动作 | 检查是否是上帝对象 |
| 字段需要不同生命周期或初始化顺序 | 拆出独立拥有者或 builder |

**禁止：**

```rust
pub struct Session {
    id: SessionId,
    config: SessionConfig,
    user: User,
    permissions: Permissions,
    socket: TcpStream,
    database: DatabasePool,
    cache: Cache,
    retry_policy: RetryPolicy,
    metrics: Metrics,
    closed: AtomicBool,
}
```

这个 `Session` 同时负责配置、用户状态、权限、网络、数据库、缓存、重试、指标和关闭状态。它会因为不同原因频繁变化，也很难测试。

**推荐：**

```rust
pub struct Session {
    id: SessionId,
    user: User,
    permissions: Permissions,
    state: SessionState,
}

pub struct SessionRuntime<R, C, M> {
    repository: R,
    cache: C,
    metrics: M,
}

pub struct SessionPolicy {
    retry: RetryPolicy,
    timeout: Duration,
}
```

领域对象 `Session` 只维护会话不变量；外部资源放进 runtime；策略放进 policy。三者可以组合使用，但不应合并成一个大结构体。

## 9.4 按职责拆分对象

拆分对象时，优先看“变化原因”，再看“技术类型”。同一变化原因的字段和方法放在一起，不同变化原因拆开。

| 拆分维度 | 说明 | 常见类型名 |
|----------|------|------------|
| 配置 | 构造后很少变化，来自文件、CLI、feature 或默认值 | `Config`、`Options` |
| 领域状态 | 业务不变量和状态转换 | `State`、`Lifecycle`、`Session` |
| 外部边界 | 数据库、网络、硬件、文件系统、时间 | `Repository`、`Adapter`、`Runtime` |
| 策略 | 可替换算法、权限、重试、调度、选择逻辑 | `Policy`、`Strategy` |
| 事件和结果 | 跨边界传递的事实 | `Event`、`Command`、`Receipt` |
| 错误 | 可匹配、可转换的失败语义 | `Error`、`Reason` |

拆分后，主流程对象应像编排函数一样组合这些能力：

```rust
pub struct ActivateUser<R, P> {
    repository: R,
    policy: P,
}

impl<R, P> ActivateUser<R, P>
where
    R: UserRepository,
    P: ActivationPolicy,
{
    pub fn execute(&self, id: UserId) -> Result<User, ActivateUserError> {
        let mut user = self.repository.load(id)?;
        self.policy.ensure_can_activate(&user)?;
        user.activate()?;
        self.repository.save(&user)?;
        Ok(user)
    }
}
```

这里的复用不是继承一个 `BaseService`，而是组合两个能力边界：读取/保存用户、判断激活策略。

## 9.5 领域对象不直接承担边界适配

领域对象负责维护不变量，不负责数据库、HTTP、文件、硬件寄存器或日志格式。

**禁止：**

```rust
impl User {
    pub async fn save_to_database(&self, pool: &PgPool) -> Result<(), sqlx::Error> {
        // database detail inside domain object
    }

    pub fn to_http_response(&self) -> HttpResponse {
        // web framework detail inside domain object
    }
}
```

**推荐：**

```rust
impl User {
    pub fn activate(&mut self) -> Result<(), UserError> {
        self.status.activate()
    }
}

pub trait UserRepository {
    fn save(&self, user: &User) -> Result<(), UserRepositoryError>;
}

pub struct UserResponse {
    id: String,
    status: String,
}

impl From<&User> for UserResponse {
    fn from(user: &User) -> Self {
        Self {
            id: user.id().to_string(),
            status: user.status().to_string(),
        }
    }
}
```

领域对象保持稳定，边界层负责把它转换成数据库行、HTTP 响应、设备描述符或用户态 ABI。

---

# 10. 并发与异步准则

## 10.1 并发共享必须显式

优先级：

1. 不共享，转移所有权；
2. channel 传消息；
3. `Arc<T>` 共享只读数据；
4. `Arc<Mutex<T>>` / `Arc<RwLock<T>>` 共享可变数据；
5. 原子类型；
6. `unsafe`，仅限无法用安全抽象表达时。

`Send` 表示值可以安全转移到其他线程，`Sync` 表示引用可安全跨线程共享；Rust API Guidelines 建议类型在可能时满足 `Send` 和 `Sync`，尤其是错误类型和跨线程 API。([Rust语言][9])

## 10.2 锁的作用域必须短

**禁止：**

```rust
let mut guard = state.lock().unwrap();
let user = external_api.fetch_user(id).await?;
guard.insert(user);
```

**推荐：**

```rust
let user = external_api.fetch_user(id).await?;

let mut guard = state.lock().unwrap();
guard.insert(user);
```

不要在持锁期间做 I/O、`.await`、复杂计算或回调外部代码。

## 10.3 async 函数中不要执行长时间阻塞操作

如果使用 Tokio，阻塞任务应通过 `spawn_blocking` 或专用线程池隔离；Tokio 文档说明，`spawn_blocking` 适用于最终会结束的有界阻塞工作，长期阻塞任务会占用阻塞线程池容量。([Docs.rs][17])

---

# 11. unsafe 准则

需要 `unsafe` 的 crate 必须满足：

* `unsafe` 块尽可能小；
* 外层提供 safe API；
* 每个 `unsafe` 块前说明安全不变量；
* `unsafe fn` 必须有 `# Safety` 文档；
* 有单元测试、属性测试或 fuzz 测试覆盖边界；
* code review 必须由熟悉 unsafe 的成员参与。

Rust 官方书明确说明，`unsafe` 并不会关闭 borrow checker；它只允许执行五类额外操作，例如解引用裸指针、调用 unsafe 函数、访问可变静态变量、实现 unsafe trait、访问 union 字段。同时官方建议保持 unsafe 块很小，并用安全抽象包裹。([Rust 文档][18])

---

# 12. 宏准则

## 12.1 能不用宏就不用宏

优先级：

1. 普通函数；
2. 泛型；
3. trait；
4. derive；
5. declarative macro；
6. procedural macro。

宏适合：

* 消除无法用函数/泛型消除的重复；
* 生成样板代码；
* 实现 DSL；
* derive 自动实现 trait。

宏不适合：

* 只是为了少写几行；
* 隐藏复杂控制流；
* 让错误信息难以理解；
* 让 IDE、rustdoc、类型推导变差。

Rust API Guidelines 对宏也有约束：宏输入语法应尽量接近输出的 Rust 语法，并且宏生成的 item 应支持属性和可见性修饰。([Rust语言][19])

---

# 13. 依赖与边界准则

## 13.1 依赖越少，边界越清晰

新增依赖必须说明：

* 为什么标准库不能满足；
* 该 crate 是否维护活跃；
* license 是否兼容；
* 是否进入公共 API；
* 是否影响编译时间、二进制大小、安全面；
* 是否已有替代依赖。

---

# 14. 日志与可诊断性准则

## 14.1 错误信息必须可定位

错误应包含：

* 失败的操作；
* 关键上下文；
* 下层错误源；
* 对用户安全的标识符。

推荐：

```rust
return Err(ConfigError::Read {
    path: path.to_path_buf(),
    source,
});
```

禁止：

```rust
return Err(Error::Message("failed".into()));
```

## 14.2 公共类型应实现 `Debug`

Rust API Guidelines 建议所有公共类型实现 `Debug`，且 `Debug` 输出不应为空。([Rust语言][21])

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserId(String);
```

---

# 15. 代码坏味道清单：Rust 版

以下任一出现，都应在 code review 中要求解释或重构。

| 坏味道                                  | Rust 版重构                        |
| ------------------------------------ | ------------------------------- |
| 函数过长                                 | 拆分为表达意图的小函数                     |
| 源文件过大                                | 按变化原因拆成领域文件，`mod.rs` 只做目录页     |
| 结构体字段过多                              | 拆成配置、状态、策略、资源、运行时适配            |
| 参数过多                                 | struct / builder / 领域对象         |
| 布尔参数                                 | enum / newtype                  |
| `String` 到处传                         | `&str` / `Cow<'_, str>` / 明确所有权 |
| 到处 `.clone()`                        | 重新审视所有权边界                       |
| 大量 `Arc<Mutex<_>>`                   | 考虑消息传递、所有权转移或状态拆分               |
| `unwrap` 出现在业务路径                     | `?`、错误转换、显式处理                   |
| `Box<dyn Trait>` 到处用                 | 能否用泛型或 enum                     |
| 巨型 trait                             | 拆成小 trait                       |
| 巨型 enum                              | 是否需要拆领域、引入 trait 或状态对象          |
| public 字段过多                          | 私有字段 + 构造函数 + 方法                |
| `mod.rs` 承载大量实现                      | 下沉到领域文件，只 re-export 稳定入口        |
| `mod utils` 泛滥                       | 按领域命名，拆到具体模块                    |
| 整体入口函数藏在文件中部或底部                    | 把主入口移到源文件开头，让读者先看到主线            |
| 编排函数夹杂底层解析、转换和 unsafe 细节             | 抽成命名步骤，让函数体像目录                  |
| 领域对象混入 IO、存储、格式化、HTTP 或硬件访问          | 拆到 repository、adapter、runtime     |
| 复用靠继承式大 trait 或共享大对象                 | 用组合、小 trait、泛型、newtype 分别复用     |
| 逐个手写大量机械 re-export                  | 对稳定出口使用 `pub use module::*;`      |
| 注释解释代码在干什么                           | 改名或拆函数                          |
| `unsafe` 分散                          | 收敛到小模块并提供 safe API              |
| 宏隐藏业务逻辑                              | 用函数/trait/generic 替代            |
| `todo!()` / `unimplemented!()` 合入主分支 | 禁止                              |
| 测试只测 happy path                      | 增加错误路径、边界条件、回归测试                |
| `#[cfg(test)] mod tests` 插在生产代码中间     | 移到源文件最后，作为附录阅读                  |
| 错误类型是 `String` 或 `()`                | 定义具体错误类型                        |
| 业务层依赖数据库/HTTP 框架类型                   | 建立边界转换层                         |

---

# 16. 测试准则

## 16.1 测试是设计的一部分，不是补丁

每个核心业务规则必须有测试。每个 bug 修复必须先补一个失败测试或回归测试。

Cargo 官方文档说明，`cargo test` 会在 `src` 中查找单元测试和文档测试，在 `tests/` 中查找集成测试；它还会编译 examples，并运行文档注释中的代码样例。([Rust 文档][16])

测试不是生产代码阅读主线的一部分，但它是行为契约的一部分。因此在单个源文件中，测试应作为“附录”放在文件最后：读者先读入口和实现，再读测试确认约束。

## 16.2 单元测试模块必须放在源文件最后

单元测试可以访问私有函数，因此适合验证内部规则、边界条件和回归场景。但 `#[cfg(test)] mod tests` 不应插在生产代码中间；它必须作为源文件最后一个 Rust item。

**推荐：**

```rust
pub fn activate_user(user: &mut User) -> Result<(), UserError> {
    ensure_user_can_be_activated(user)?;
    user.status = UserStatus::Active;
    Ok(())
}

fn ensure_user_can_be_activated(user: &User) -> Result<(), UserError> {
    if user.status == UserStatus::Banned {
        return Err(UserError::BannedUserCannotBeActivated);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_banned_user_activation() {
        let mut user = User::banned();

        let result = activate_user(&mut user);

        assert!(matches!(
            result,
            Err(UserError::BannedUserCannotBeActivated)
        ));
    }
}
```

测试 helper 如果只服务于测试，应放在 `mod tests` 内部。不要为了测试方便，把测试构造器、mock 或断言 helper 暴露到生产模块顶部，破坏生产代码的报纸式阅读顺序。

## 16.3 测试内部按行为组织

测试模块内部也应可阅读。推荐顺序：

1. `use super::*` 和测试专用依赖。
2. 最重要的 happy path 或主行为测试。
3. 错误路径、边界条件、回归测试。
4. 测试 helper、fixture builder、断言辅助函数。

如果测试 helper 很多，说明该行为可能需要独立的测试工具模块；但这个工具模块仍应服务测试，不应反向影响生产 API。

## 16.4 集成测试验证公共 API

```text
tests/
  create_user.rs
  checkout.rs
  payment_callback.rs
```

集成测试应像外部用户一样使用 crate，不依赖内部私有实现。它们验证的是公开 API、feature 组合、错误语义和跨模块协作。

## 16.5 测试名称必须表达场景和期望

推荐格式：

```rust
#[test]
fn returns_not_found_when_user_does_not_exist() {}

#[test]
fn rejects_order_when_inventory_is_insufficient() {}

#[test]
fn retries_payment_gateway_on_transient_error() {}
```

禁止：

```rust
#[test]
fn test1() {}

#[test]
fn user_test() {}
```

测试名称应让读者不用打开函数体，也能知道场景、动作和期望结果。

---

# 17. Pull Request 检查清单

每个 PR 必须回答以下问题：

1. **命名是否表达业务意图？**
2. **源文件开头是否能看到整体功能入口？**
3. **编排函数是否像目录一样列出可读步骤？**
4. **源文件和结构体是否按职责拆分，没有出现大文件或上帝对象？**
5. **目录是否符合 Cargo/Rust 约定，`lib.rs` / `mod.rs` 是否只做目录页和稳定出口？**
6. **re-export 是否简洁且边界清晰，是否可用 `pub use module::*;` 避免机械重复？**
7. **函数是否短小且只做一件事？**
8. **是否用类型表达了关键约束？**
9. **是否避免了不必要的 clone、共享可变状态和 `Arc<Mutex<_>>`？**
10. **错误是否使用 `Result` 显式传播？**
11. **生产路径是否没有裸 `unwrap`？**
12. **公共 API 是否隐藏了实现细节？**
13. **trait 是否小而稳定？**
14. **是否用组合、trait、泛型、newtype 分别处理复用，而不是制造共享大对象？**
15. **`#[cfg(test)] mod tests` 是否位于源文件最后？**
16. **是否有单元测试、集成测试或文档测试覆盖核心路径？**
17. **是否通过 `cargo fmt`、`cargo clippy`、`cargo test`？**
18. **如果使用 unsafe，是否有安全说明和边界测试？**

---

# 18. 推荐 CI 基线

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

库项目可额外加：

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

---

# 19. Rust 版核心口号

> **入口在前、目录式编排、小文件、小结构体、好名字、清边界、强类型、显式错误、少共享、少宏、无裸 unwrap、unsafe 收口、组合优先、测试护航。**

更具体地说：

> **不要用继承复用代码；用组合复用状态，用 trait 复用行为，用 enum 表达封闭变化，用泛型表达静态多态，用 `dyn Trait` 表达运行期开放扩展。**

公共出口可以简洁：

> **先把模块边界设计清楚，再用 `pub use module::*;` 聚合稳定入口；不要用 re-export 掩盖混乱职责。**

再落到源文件阅读体验上：

> **先让读者看到整体功能，再用一个个可命名的函数调用展开细节；生产代码像正文，测试像附录，附录放在最后。**

[1]: https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html "Understanding Ownership - The Rust Programming Language"
[2]: https://rust-lang.github.io/api-guidelines/naming.html "Naming - Rust API Guidelines"
[3]: https://rust-lang.github.io/api-guidelines/type-safety.html "Type safety - Rust API Guidelines"
[4]: https://rust-lang.github.io/api-guidelines/documentation.html "Documentation - Rust API Guidelines"
[5]: https://doc.rust-lang.org/style-guide/ "Introduction - The Rust Style Guide"
[6]: https://doc.rust-lang.org/clippy/ "Introduction - Clippy Documentation"
[7]: https://doc.rust-lang.org/cargo/reference/workspaces.html "Workspaces - The Cargo Book"
[8]: https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html "Recoverable Errors with Result - The Rust Programming Language"
[9]: https://rust-lang.github.io/api-guidelines/interoperability.html "Interoperability - Rust API Guidelines"
[10]: https://rust-lang.github.io/api-guidelines/future-proofing.html "Future proofing - Rust API Guidelines"
[11]: https://doc.rust-lang.org/reference/visibility-and-privacy.html "Visibility and privacy - The Rust Reference"
[12]: https://doc.rust-lang.org/book/ch18-01-what-is-oo.html "Characteristics of Object-Oriented Languages - The Rust Programming Language"
[13]: https://doc.rust-lang.org/book/ch10-02-traits.html "Defining Shared Behavior with Traits - The Rust Programming Language"
[14]: https://rust-lang.github.io/api-guidelines/checklist.html "Checklist - Rust API Guidelines"
[15]: https://doc.rust-lang.org/book/ch18-03-oo-design-patterns.html "Implementing an Object-Oriented Design Pattern"
[16]: https://doc.rust-lang.org/cargo/guide/tests.html "Tests - The Cargo Book"
[17]: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html "spawn_blocking in tokio::task - Rust"
[18]: https://doc.rust-lang.org/book/ch20-01-unsafe-rust.html "Unsafe Rust - The Rust Programming Language"
[19]: https://rust-lang.github.io/api-guidelines/macros.html "Macros - Rust API Guidelines"
[20]: https://rust-lang.github.io/api-guidelines/necessities.html "Necessities - Rust API Guidelines"
[21]: https://rust-lang.github.io/api-guidelines/debuggability.html "Debuggability - Rust API Guidelines"
[22]: https://doc.rust-lang.org/cargo/guide/project-layout.html "Package Layout - The Cargo Book"
[23]: https://doc.rust-lang.org/book/ch07-05-separating-modules-into-different-files.html "Separating Modules into Different Files - The Rust Programming Language"
[24]: https://blog.cleancoder.com/uncle-bob/2014/05/08/SingleReponsibilityPrinciple.html "The Single Responsibility Principle - The Clean Coder Blog"
