# axvisor\_api （实验性下一代 Axvisor API）

**⚠️这个仓库是实验性的，API的内容和语法可能发生变动⚠️**

**⚠️这些API可能最终转正，也可能在未来被删除⚠️**

**⚠️维护者会尽力保持API的兼容性，但 breaking changes 仍可能发生⚠️**

## 为什么需要新一代 API？

Axvisor 的各个部件需要访问 ArceOS 系统提供的功能。对于 Axvisor 本体，ArceOS 是它的依赖项，它可以直接访问 ArceOS 的 API；然而出于解耦合的考虑，其他位于更下层的组件不应该将 ArceOS 作为自己的依赖项，也就不能直接访问 ArceOS 的 API；因此，需要一种“依赖注入”的方式来将 ArceOS 的 API 提供给 Axvisor 的各个部件。

目前 Axvisor 中主要是通过 Trait + 泛型参数来实现“依赖注入”，给各个组件提供 API 的，例如：

```rust
// 组件定义自己需要的 API
pub trait ModAHal {
    fn foo() -> u32;
}

pub struct ModA<T: ModAHal> {
    state: u32,
}

impl<T: ModAHal> ModA<T> {
    pub fn new() -> Self {
        Self { state: T::foo() }
    }
}

// Axvisor 提供实现
pub struct ModAHalImpl;

impl ModAHal for ModAHalImpl {
    fn foo() -> u32 {
        42
    }
}

pub fn main() {
    let mod_a = ModA::<ModAHalImpl>::new();
    println!("ModA state: {}", mod_a.state);
}
```

这种方法的好处是显而易见的：

1. 非常优雅，完全符合 Rust 的编程范式，没有任何黑魔法，也容易理解；
2. 耦合性低，理论上可以将任何底层组件移植到任何其他 Kernel 上，只要它们能提供对应的 API 实现；

然而这样的做法也有缺点：

1. 引用者（结构体或函数）必须带着它使用的所有依赖项（结构体或函数）的泛型参数，这会导致代码冗长，降低可读性；
2. 不同的 Trait 之间难免有重复的方法，导致代码冗余；
3. 前两个问题有一个共同的解决方式，那就是对 API 进行分组归类，然而这又会导致 Trait 之间的嵌套和耦合关系增加，例如：
   
    ```rust
    pub trait MemoryHal {
        // 内存相关的 API
    }

    pub trait VCpuHal {
        type Memory: MemoryHal;
        // 虚拟 CPU 相关的 API
    }

    pub trait VMHal {
        type VCpu: VCpuHal;
        // 虚拟机相关的 API
    }
    ```

4. 最严重的问题在于，如果位于依赖图末端的某个结构体或方法增加了一个依赖的 API，那么它的所有上游使用者的类型签名都必须修改以适应这个变化，这会导致代码的维护成本大幅增加。

## 新一代 API 的设计

`axvisor_api` 旨在解决上述问题。它的设计思路是：

1. 使用 `crate_interface` 来定义 API 的接口，并且将 `crate_interface` 提供的 API 包装成普通的函数；
2. 以模块为单位组织 API，一个模块对应一个功能方向，对应一个 `crate_interface` Trait；
3. 每个模块内部，除了 API 函数的定义以外，还可以包含类型定义、常量和基于 API 函数实现的其他函数等；

`axvisor_api` 的示例代码如下：

```rust
// 定义一个 API 模块
#[api_mod]
mod memory {
    pub use memory_addr::{PhysAddr, VirtAddr};

    /// Allocate a frame.
    extern fn alloc_frame() -> Option<PhysAddr>;
    /// Deallocate a frame.
    extern fn dealloc_frame(addr: PhysAddr);
}

// 实现 API 模块
#[api_mod_impl(axvisor_api::memory)]
mod memory_impl {
    use crate_interface::memory::{alloc_frame, dealloc_frame, PhysAddr};

    extern fn alloc_frame() -> Option<PhysAddr> {
        // 调用 ArceOS 的内存分配函数
        arceos_memory_alloc()
    }

    extern fn dealloc_frame(addr: PhysAddr) {
        // 调用 ArceOS 的内存释放函数
        arceos_memory_dealloc(addr);
    }
}

// 使用 API 模块
use axvisor_api::memory::{alloc_frame, dealloc_frame, PhysAddr};
pub fn main() {
    let frame = alloc_frame().expect("Failed to allocate frame");
    println!("Allocated frame at address: {:?}", frame);
    dealloc_frame(frame);
}
```

可以说，这是通过一个统一且按功能分类的 API 集合取代了之前的所有 Trait，并且通过 `crate_interface` 取消了对 Trait 的显式依赖。这样的实现的优势在于：

1. API 函数的调用方式与普通函数一致，使用起来更简单，降低了使用者的心智负担；
2. 调用者无需关系其依赖项需要哪些 API；依赖项的修改不会影响调用者；
3. API 模块可以包含类型定义、常量和其他函数等，提供了更好的组织方式；

同样地，这样的设计也有一些缺点：

1. 虽然 `crate_interface` 背后使用 Trait 实现了一定的编译期检查，但是相比于之前的 Trait 方式，编译期检查的能力有所下降；例如如果一个 `api_mod` 没有被实现，只有在链接时才能发现，而不是编译期；
2. 这样设计本质上不允许通过不同的 Trait 实现，在同一个程序中为同一个组件提供两种不同的 API 实现；这在一定程度上损失了灵活性；不过这种情况在 Axvisor 中并不常见，目前也没有造成实际问题；
3. 降低了单个组件直接复用的能力；例如某个组件原本可以直接被其他组件复用，现在需要额外引入 `axvisor_api` 的依赖；虽然实际上可以通过 feature 来关闭 `axvisor_api` 中一部分没有用到的 API（目前未实现），但这仍然是一个缺点；

## 当前仍存在的问题

除了以上提到的缺点，`axvisor_api` 目前的具体实现仍然存在一些问题：

1. **不能支持非内联模块**：对于最常见的，将模块放置在单独的文件中的方式，`axvisor_api` 目前还不支持；即定义 `#[api_mod] mod x;` 然后在 `x.rs` 中定义 API 模块的方式是无法工作的；这是 Rust 过程宏的能力所限；
2. **对 IDE 的功能有轻微干扰**：由于 `axvisor_api` 使用了过程宏，可能会导致某些 IDE 的代码补全和跳转功能不如预期；不过目前测试 rust-analyzer 工作比较正常；
3. **`extern fn` 语法和 rustfmt 的冲突**：使用 `extern fn` 标记 API 函数是出于可读性和语法一致性的考虑，但是 rustfmt 会将其格式化为 `extern "C" fn`，这会导致编译错误；可能的解决办法包括直接使用 `extern "C" fn`，但这会和真正的外部 C 函数声明冲突；
4. **API 函数不够醒目**：由于 API 函数和普通函数的区别在于 `extern` 关键字和没有函数体，在大段代码中可能不够醒目；目前通过在生成的文档中给出详细列表的方式，试图弥补这一点；未来可以考虑使用 `#[api]` 属性来标记 API 函数，以提高可读性；

此外，还有一些问题和 `axvisor_api` 的设计无关和实现无关，属于无论怎样设计 API 接口都会遇到的问题，此处也列出一条作者能想到的：

- **平台相关 API**：某些 API 是平台强相关，甚至具体设备强相关的，但又是非常必要的。例如，ARM 架构下 GIC 的半虚拟化实现，就与物理 GIC 驱动的实现密切相关，前者需要调用很多后者的功能；将这些功能全部放置于 `axvisor_api` 中会导致 API 模块过于臃肿；但不统一在一起又可能造成可读性和可维护性下降，容易出错等等问题；
