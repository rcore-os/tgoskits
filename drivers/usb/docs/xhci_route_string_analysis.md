# xHCI Route String 分析与实现

## 概述

本文档记录了 xHCI Route String 的规范理解、实现细节和常见问题，避免未来开发中的误解。

## xHCI 规范要求

### Route String 定义（xHCI Spec Section 4.8.5, USB 3.2 Spec Section 8.9）

**关键原则：Route String 不包括 Root Hub Port Number**

Route String 是一个 20 位字段，用于记录从 Root Hub 到设备的 Hub 层级路径：

```
Bits 19:16 | Bits 15:12 | Bits 11:8 | Bits 7:4 | Bits 3:0
   Tier 5   |   Tier 4   |   Tier 3  |  Tier 2  |  Tier 1
  (4-bit)  |  (4-bit)  |  (4-bit)  | (4-bit)  | (4-bit)
```

- **Tier 1**: 第一个 Hub 的端口号
- **Tier 2-5**: 后续 Hub 层级的端口号
- **值为 0**: 表示该层级无 Hub

### 设备类型与 Route String

#### 1. 直连 Root Hub 的设备

```
Root Hub Port N
  └─ Device
```

- **Route String** = `0x0` (无 Hub 层级)
- **Root Hub Port Number** = N

#### 2. 通过一个 Hub 连接的设备

```
Root Hub Port N
  └─ Hub (Port M)
      └─ Device
```

- **Route String** = `0xP` (Tier 1 = P，表示在 Hub Port P)
- **Root Hub Port Number** = N (继承父 Hub 的 Root Hub Port)
- **注意**: Hub 本身的 Route String = 0x0

#### 3. 通过多个 Hub 连接的设备

```
Root Hub Port N
  └─ Hub1 (Port M)
      └─ Hub2 (Port P)
          └─ Device
```

- **Route String** = `0xPM` (Tier 1 = M, Tier 2 = P)
- **Root Hub Port Number** = N

### 实际案例分析

#### 案例 1: QEMU 测试（直连设备）

```
Root Hub Port 5
  └─ Low Speed Device
```

期望值：

- Route String = `0x0` ✓
- Root Hub Port Number = `5`

实际输出：

```
Address device SlotId(1)
    root port: 5
    route string: 0x0
```

✅ **正确**

#### 案例 2: 真机测试（External Hub）

```
Root Hub Port 1
  └─ High Speed Hub (SlotId=1) (Port 4)
      └─ High Speed Device
```

期望值：

- Hub 的 Route String = `0x0` (直连 Root Hub)
- Device 的 Route String = `0x4` (Tier 1 = 4)
- Root Hub Port Number = `1` (两个设备都相同)

实际输出：

```
Address device SlotId(2)
    root port: 1
    route string: 0x4
    parent_hub_slot_id: 1
```

✅ **正确**（Route String 不包括 Root Hub）

## 代码实现

### 当前实现（usb-host/src/kcore.rs:56-62）

```rust
let route_string = if let Some(route) = &self.hubs.get(id).unwrap().route_string {
    // External Hub 的子设备：继承父 Hub 的 route_string 并 push 当前端口
    let mut rs = *route;
    rs.push_hub(addr_info.port_id);
    rs
} else {
    // Root Hub 的子设备（直连或 External Hub）：route_string = 0x0
    RouteString::follow_root()
};
```

**关键点**：

- `RouteString::follow_root()` 返回 `0x0`
- 对于直连设备（无 Hub），route_string = `0x0` ✓
- 对于 External Hub，route_string = `0x0` ✓（因为直连 Root Hub）
- 对于 External Hub 的子设备，从父 Hub 的 route_string 开始 push

### RouteString::push_hub 实现（usb-host/src/hub/mod.rs:37-53）

```rust
pub fn push_hub(&mut self, hub_port: u8) {
    assert!(hub_port <= 15);
    let mut target_depth = None;
    for depth in 1..=5 {
        let shift = (depth - 1) * 4;
        let port = (self.0 >> shift) & 0x0F;
        if port == 0 {
            target_depth = Some(depth);
            break;
        }
    }

    let depth = target_depth.expect("route string is full");
    let shift = (depth - 1) * 4;
    let mask = 0x0F << shift;
    self.0 = (self.0 & !mask) | (((hub_port as u32) & 0x0F) << shift);
}
```

**行为**：

- 找到第一个为 0 的深度（Tier）
- 在该 Tier 设置端口号
- 例如：
  - 从 `0x0` push_hub(1) → `0x1`
  - 从 `0x1` push_hub(4) → `0x14`

## 常见误解与陷阱

### ❌ 错误理解：External Hub 需要 push_hub

**错误想法**：External Hub 连接在 Root Hub Port N，所以它的 route_string 应该包含 N。

**正确理解**：

- Route String **只记录 Hub 层级**，不包括 Root Hub
- External Hub 虽然连接在 Root Hub Port 1，但它前面没有其他 Hub
- 所以 External Hub 的 route_string = `0x0`
- Root Hub Port Number 是单独的字段

**如果错误地 push_hub**：

```rust
// 错误示例
let mut rs = RouteString::follow_root();  // 0x0
rs.push_hub(5);  // 变成 0x5

// 对于 Root Hub Port 5 的直连设备：
// - Route String = 0x5  ✗ 错误！应该是 0x0
// - 导致 QEMU 测试崩溃（TrbError）
```

### ❌ 错误理解：Device 的 route_string 应该包含 Root Hub Port

**错误想法**：Device 在 Root Hub Port 1 → Hub Port 4，所以 route_string 应该是 `0x14`（1 和 4）。

**正确理解**：

- Route String = `0x4`（只有 Hub Port 4）
- Root Hub Port Number = `1`（单独字段）
- 这两个字段分别记录拓扑信息

### ❌ 错误理解：route_string = 0x4 表示错误

从真机测试输出看到 `route string: 0x4` 时，可能会误以为是错误的（因为父 Hub 在 Port 1）。

**正确理解**：

- `0x4` 表示 Tier 1 = 4（设备在第一层 Hub 的 Port 4）
- 这是**完全正确**的值
- 父 Hub 的 Root Hub Port Number = 1 是单独记录的

## Debug 技巧

### 查看完整的拓扑路径

```rust
let port_path = route_string.to_port_path_string(root_port_id);
// 输出: "1.4" (Root Hub Port 1 → Hub Port 4)
```

### 追踪 route_string 的传递

在 `kcore.rs` 中添加的 debug 日志：

```rust
debug!(
    "Parent Hub({:?}): route_string={:?}, slot_id={}",
    id, parent_route_string, parent_hub_id
);

debug!(
    "Device route_string: parent_route={:#x}, port_id={}, new_route={:#x}",
    route.raw(),
    addr_info.port_id,
    rs.raw()
);
```

### Debug trait 的显示问题

```rust
impl Debug for RouteString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut iter = self.route_port_ids();
        if let Some(first) = iter.next() {
            write!(f, "{first}")?;
            for port in iter {
                write!(f, ".{port}")?;
            }
        }
        Ok(())
    }
}
```

**注意**：

- 如果 `route_string = 0x0`，`route_port_ids()` 返回空迭代器
- Debug 输出显示为空字符串，而不是 `0x0`
- 所以 `route_string=Some()` 可能实际上是 `Some(0x0)`

**建议**：

- 使用 `.raw()` 方法查看原始值：`route_string.raw()`
- 输出格式：`{:#x}` 显示十六进制

## 相关文档

- **xHCI Specification**: Section 4.8.5 (Route String)
- **USB 3.2 Specification**: Section 8.9 (Route String Field)
- **U-Boot**: `drivers/usb/host/xhci.c` (xhci_update_hub_device)

## 修订历史

- 2025-01-22: 初始版本，记录 Route String 规范理解和常见误解
- 修正了关于 External Hub route_string 的错误理解
- 添加了 QEMU 和真机测试案例分析
