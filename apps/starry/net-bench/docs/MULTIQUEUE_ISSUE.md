# 多队列问题处理说明

## 问题描述

在使用 vhost-net + TAP 网络时，遇到多队列配置错误：

```
qemu-system-x86_64: -netdev tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on,queues=4: 
could not configure /dev/net/tun (tap0): Invalid argument
```

---

## 根本原因

1. **TAP 设备默认不支持多队列**
   - 使用 `ip tuntap add` 创建的 TAP 默认是单队列
   - QEMU 参数中指定 `queues=4` 会失败

2. **多队列需要特殊创建**
   ```bash
   # 错误：默认单队列
   ip tuntap add mode tap tap0
   
   # 正确：支持多队列
   ip tuntap add mode tap tap0 multi_queue
   ```

---

## 解决方案

### 方案 1：移除多队列参数（已采用）

**修改前**（旧配置）：
```toml
"-device", "virtio-net-pci,netdev=net0,mac=...,mq=on,vectors=10,csum=on,gso=on,host_tso4=on,host_tso6=on,guest_tso4=on,guest_tso6=on",
"-netdev", "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on,queues=4",
```

**修改后**（新配置）：
```toml
"-device", "virtio-net-pci,netdev=net0,mac=...",
"-netdev", "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on",
```

**优点**：
- 简单可靠，立即生效
- 兼容所有环境
- 无需修改 TAP 创建逻辑

**缺点**：
- 性能受限于单队列（但对于基线测试足够）

---

### 方案 2：创建多队列 TAP（未采用）

需要修改 `env/setup-common.sh` 中的 TAP 创建：

```bash
# 创建支持多队列的 TAP
setup_tap() {
    if tap_exists; then
        warn "TAP 设备 $TAP_DEVICE 已存在，跳过创建"
        return 0
    fi
    
    info "创建多队列 TAP 设备 $TAP_DEVICE (queues=4)"
    
    # 添加 multi_queue 参数
    ip tuntap add mode tap "$TAP_DEVICE" multi_queue || die "创建 TAP 设备失败"
    ip link set "$TAP_DEVICE" up || die "启动 TAP 设备失败"
    ip link set "$TAP_DEVICE" master "$BRIDGE" || die "挂载 TAP 到 bridge 失败"
    
    record_resource "tap" "$TAP_DEVICE" "master=$BRIDGE,queues=4"
    info "多队列 TAP 设备 $TAP_DEVICE 创建成功"
}
```

**优点**：
- 支持多队列，理论性能更好
- 可以测试多核网络扩展

**缺点**：
- 需要内核支持（较新内核）
- 配置更复杂
- Starry 当前是单队列，多队列优势无法发挥

---

## 已修改的文件

### 1. `qemu/vhost-x86_64-kvm.toml`
```toml
# 移除了多队列参数
"-device", "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:57",
"-netdev", "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on",
```

### 2. `qemu/vhost-x86_64-tcg.toml`
```toml
# 保持简化配置
"-device", "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:57",
"-netdev", "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on",
```

### 3. `qemu/vhost-aarch64-kvm.toml`
```toml
# 移除了多队列参数
"-device", "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:56",
"-netdev", "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on",
```

### 4. `qemu/vhost-aarch64-tcg.toml`
```toml
# 保持简化配置（单队列）
"-device", "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:56",
"-netdev", "tap,id=net0,ifname=tap0,script=no,downscript=no,vhost=on",
```

---

## 测试验证

### x86_64 + KVM
- 移除多队列参数后，QEMU 启动成功
- 但遇到其他问题（网络设备未找到，rootfs 脚本缺失）

### aarch64 + TCG（WSL2）
- 完全通过，测试成功
- 已验证 5 种测试场景全部通过

---

## 未来改进（可选）

如果需要测试多队列性能：

1. **修改 TAP 创建逻辑**
   - 在 `env/setup-common.sh` 中添加 `multi_queue` 参数
   - 根据场景决定是否启用多队列

2. **提供多队列专用配置**
   ```
   qemu/vhost-smp4-x86_64-kvm.toml  # 多队列 + 多核
   qemu/vhost-x86_64-kvm.toml       # 单队列（兼容性）
   ```

3. **自动检测内核支持**
   ```bash
   # 检测内核是否支持多队列 TAP
   if ip tuntap add mode tap test-mq multi_queue 2>/dev/null; then
       ip tuntap del mode tap test-mq
       echo "支持多队列"
   else
       echo "不支持多队列，使用单队列"
   fi
   ```

---

## 总结

**当前策略**：使用单队列配置
- 简单可靠
- 兼容性好
- 满足基线测试需求

**原始配置的多队列参数**：
- 对应高性能多核扩展测试场景
- 适合多核扩展测试
- 但需要 TAP 设备支持（需要 `multi_queue` 创建）

**实际处理**：
- 为了快速验证和兼容性，采用单队列配置
- 保留了未来添加多队列支持的可能性
- 文档中标注了多队列的实现方案
