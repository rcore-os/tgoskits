# USB 键盘事件解析

这个模块使用 `keyboard-types` crate 实现了 USB HID 键盘的事件解析功能。

## 功能特性

- 🔍 自动检测 USB HID 键盘设备
- ⌨️ 解析按键按下和释放事件
- 🎛️ 支持修饰键（Ctrl、Shift、Alt、Meta）
- 📊 跟踪当前按下的所有键
- 🗂️ 完整的扫描码到键值映射

## 使用方法

### 基本用法

```rust
use usb_keyboard::{KeyBoard, KeyEvent};
use crab_usb::DeviceList;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 查找键盘设备
    let device_list = DeviceList::new()?;
    for device_info in device_list.iter() {
        if KeyBoard::check(&device_info) {
            let device = device_info.open().await?;
            let mut keyboard = KeyBoard::new(device).await?;
            
            // 监听键盘事件
            loop {
                let events = keyboard.recv_events().await?;
                for event in events {
                    match event {
                        KeyEvent::KeyDown { key, modifiers } => {
                            println!("按下: {:?} {:?}", key, modifiers);
                        }
                        KeyEvent::KeyUp { key, modifiers } => {
                            println!("释放: {:?} {:?}", key, modifiers);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
```

### 高级用法

```rust
// 获取当前按下的所有键
let pressed_keys = keyboard.get_pressed_keys();
println!("当前按下的键: {:?}", pressed_keys);

// 获取当前修饰键状态
let modifiers = keyboard.get_modifiers();
if modifiers.contains(keyboard_types::Modifiers::CONTROL) {
    println!("Ctrl 键被按下");
}
```

## 支持的按键

### 字母键
- A-Z (a-z)

### 数字键
- 0-9

### 功能键
- F1-F12
- Enter, Escape, Backspace, Tab
- Space, CapsLock
- 方向键（↑↓←→）

### 修饰键
- Ctrl (左右)
- Shift (左右)
- Alt (左右)
- Meta/Windows/Cmd (左右)

### 特殊键
- Insert, Delete, Home, End
- Page Up, Page Down
- Print Screen, Scroll Lock, Pause

### 标点符号
- `-` `=` `[` `]` `\\`
- `;` `'` `` ` ``
- `,` `.` `/`

## 运行示例

```bash
# 基本键盘事件监听
cargo run --example simple_keyboard

# 详细的键盘事件监听（包含调试信息）
cargo run --example keyboard_events
```

## 注意事项

1. **权限要求**: 在 Linux 系统上可能需要 root 权限或正确的 udev 规则来访问 USB 设备
2. **设备兼容性**: 支持标准的 USB HID 键盘协议
3. **异步操作**: 所有操作都是异步的，需要在 async 环境中运行

## USB HID 键盘报告格式

USB HID 键盘使用 8 字节的报告格式：

```
Byte 0: 修饰键状态位图
Byte 1: 保留（通常为 0）
Byte 2-7: 按键扫描码（最多 6 个同时按下的键）
```

修饰键位图：
- Bit 0: Left Ctrl
- Bit 1: Left Shift  
- Bit 2: Left Alt
- Bit 3: Left GUI (Windows/Cmd)
- Bit 4: Right Ctrl
- Bit 5: Right Shift
- Bit 6: Right Alt
- Bit 7: Right GUI (Windows/Cmd)

## 依赖

- `keyboard-types`: 键盘类型和修饰键定义
- `crab-usb`: USB 设备通信
- `log`: 日志记录
