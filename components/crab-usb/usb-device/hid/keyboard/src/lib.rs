#![no_std]

extern crate alloc;
use alloc::{string::ToString, vec::Vec};

use anyhow::bail;
use crab_usb::{
    Endpoint,
    device::{Device, DeviceInfo},
    err::USBError,
};
use keyboard_types::{Key, Modifiers, NamedKey};
use log::debug;
use usb_if::{
    descriptor::{Class, EndpointType},
    endpoint::TransferRequest,
    transfer::Direction,
};

/// 键盘事件类型
#[derive(Debug, Clone, PartialEq)]
pub enum KeyEvent {
    /// 按键按下事件
    KeyDown { key: Key, modifiers: Modifiers },
    /// 按键释放事件
    KeyUp { key: Key, modifiers: Modifiers },
}

/// USB HID 键盘扫描码到 Key 的映射
fn scancode_to_key(scancode: u8) -> Option<Key> {
    match scancode {
        0x04 => Some(Key::Character("a".to_string())),
        0x05 => Some(Key::Character("b".to_string())),
        0x06 => Some(Key::Character("c".to_string())),
        0x07 => Some(Key::Character("d".to_string())),
        0x08 => Some(Key::Character("e".to_string())),
        0x09 => Some(Key::Character("f".to_string())),
        0x0A => Some(Key::Character("g".to_string())),
        0x0B => Some(Key::Character("h".to_string())),
        0x0C => Some(Key::Character("i".to_string())),
        0x0D => Some(Key::Character("j".to_string())),
        0x0E => Some(Key::Character("k".to_string())),
        0x0F => Some(Key::Character("l".to_string())),
        0x10 => Some(Key::Character("m".to_string())),
        0x11 => Some(Key::Character("n".to_string())),
        0x12 => Some(Key::Character("o".to_string())),
        0x13 => Some(Key::Character("p".to_string())),
        0x14 => Some(Key::Character("q".to_string())),
        0x15 => Some(Key::Character("r".to_string())),
        0x16 => Some(Key::Character("s".to_string())),
        0x17 => Some(Key::Character("t".to_string())),
        0x18 => Some(Key::Character("u".to_string())),
        0x19 => Some(Key::Character("v".to_string())),
        0x1A => Some(Key::Character("w".to_string())),
        0x1B => Some(Key::Character("x".to_string())),
        0x1C => Some(Key::Character("y".to_string())),
        0x1D => Some(Key::Character("z".to_string())),

        // 数字键
        0x1E => Some(Key::Character("1".to_string())),
        0x1F => Some(Key::Character("2".to_string())),
        0x20 => Some(Key::Character("3".to_string())),
        0x21 => Some(Key::Character("4".to_string())),
        0x22 => Some(Key::Character("5".to_string())),
        0x23 => Some(Key::Character("6".to_string())),
        0x24 => Some(Key::Character("7".to_string())),
        0x25 => Some(Key::Character("8".to_string())),
        0x26 => Some(Key::Character("9".to_string())),
        0x27 => Some(Key::Character("0".to_string())),

        // 特殊键
        0x28 => Some(Key::Named(NamedKey::Enter)),
        0x29 => Some(Key::Named(NamedKey::Escape)),
        0x2A => Some(Key::Named(NamedKey::Backspace)),
        0x2B => Some(Key::Named(NamedKey::Tab)),
        0x2C => Some(Key::Character(" ".to_string())), // Space
        0x2D => Some(Key::Character("-".to_string())),
        0x2E => Some(Key::Character("=".to_string())),
        0x2F => Some(Key::Character("[".to_string())),
        0x30 => Some(Key::Character("]".to_string())),
        0x31 => Some(Key::Character("\\".to_string())),
        0x33 => Some(Key::Character(";".to_string())),
        0x34 => Some(Key::Character("'".to_string())),
        0x35 => Some(Key::Character("`".to_string())),
        0x36 => Some(Key::Character(",".to_string())),
        0x37 => Some(Key::Character(".".to_string())),
        0x38 => Some(Key::Character("/".to_string())),

        // 功能键
        0x3A => Some(Key::Named(NamedKey::F1)),
        0x3B => Some(Key::Named(NamedKey::F2)),
        0x3C => Some(Key::Named(NamedKey::F3)),
        0x3D => Some(Key::Named(NamedKey::F4)),
        0x3E => Some(Key::Named(NamedKey::F5)),
        0x3F => Some(Key::Named(NamedKey::F6)),
        0x40 => Some(Key::Named(NamedKey::F7)),
        0x41 => Some(Key::Named(NamedKey::F8)),
        0x42 => Some(Key::Named(NamedKey::F9)),
        0x43 => Some(Key::Named(NamedKey::F10)),
        0x44 => Some(Key::Named(NamedKey::F11)),
        0x45 => Some(Key::Named(NamedKey::F12)),

        // 方向键
        0x4F => Some(Key::Named(NamedKey::ArrowRight)),
        0x50 => Some(Key::Named(NamedKey::ArrowLeft)),
        0x51 => Some(Key::Named(NamedKey::ArrowDown)),
        0x52 => Some(Key::Named(NamedKey::ArrowUp)),

        // 其他常用键
        0x39 => Some(Key::Named(NamedKey::CapsLock)),
        0x46 => Some(Key::Named(NamedKey::PrintScreen)),
        0x47 => Some(Key::Named(NamedKey::ScrollLock)),
        0x48 => Some(Key::Named(NamedKey::Pause)),
        0x49 => Some(Key::Named(NamedKey::Insert)),
        0x4A => Some(Key::Named(NamedKey::Home)),
        0x4B => Some(Key::Named(NamedKey::PageUp)),
        0x4C => Some(Key::Named(NamedKey::Delete)),
        0x4D => Some(Key::Named(NamedKey::End)),
        0x4E => Some(Key::Named(NamedKey::PageDown)),

        _ => None,
    }
}

pub struct KeyBoard {
    _device: Device,
    endpoint: Endpoint,
    /// 上一次按键状态，用于检测按键变化
    previous_state: [u8; 8],
}

impl KeyBoard {
    /// 检查设备是否为 HID 键盘设备
    pub fn check(info: &DeviceInfo) -> bool {
        for config in info.configurations() {
            for interface in &config.interfaces {
                let alt = interface.first_alt_setting();
                if matches!(alt.class(), Class::Hid) && alt.subclass == 1 && alt.protocol == 1 {
                    return true;
                }
            }
        }
        false
    }

    /// 创建新的键盘设备实例
    pub async fn new(mut device: Device) -> Result<Self, USBError> {
        for config in device.configurations() {
            debug!("Configuration: {config:?}");
        }

        // 查找 HID 键盘接口
        let config = &device.configurations()[0];
        let (interface_number, alternate_setting, endpoint_address) = config
            .interfaces
            .iter()
            .find_map(|iface| {
                let alt = iface.first_alt_setting();
                if matches!(alt.class(), Class::Hid) && alt.subclass == 1 && alt.protocol == 1 {
                    // 查找中断 IN 端点
                    for ep in &alt.endpoints {
                        if matches!(ep.transfer_type, EndpointType::Interrupt)
                            && matches!(ep.direction, Direction::In)
                        {
                            return Some((alt.interface_number, alt.alternate_setting, ep.address));
                        }
                    }
                }
                None
            })
            .ok_or(USBError::NotFound)?;

        debug!(
            "Using interface: {interface_number}, alt: {alternate_setting}, endpoint: {endpoint_address:#x}"
        );

        // 声明接口
        device
            .claim_interface(interface_number, alternate_setting)
            .await?;

        let endpoint = device.endpoint(endpoint_address)?;

        Ok(Self {
            _device: device,
            endpoint,
            previous_state: [0; 8],
        })
    }

    /// 接收并解析键盘事件
    pub async fn recv_events(&mut self) -> Result<Vec<KeyEvent>, anyhow::Error> {
        let mut buf = [0u8; 8];
        let n = self
            .endpoint
            .wait(TransferRequest::interrupt_in(&mut buf))
            .await?
            .actual_length;

        if n == 0 {
            bail!("No data received from keyboard");
        }

        let events = self.parse_keyboard_report(&buf);
        self.previous_state = buf;
        Ok(events)
    }

    /// 解析 USB HID 键盘报告
    fn parse_keyboard_report(&self, report: &[u8; 8]) -> Vec<KeyEvent> {
        let mut events = Vec::new();

        if report.len() < 8 {
            return events;
        }

        // USB HID 键盘报告格式:
        // Byte 0: 修饰键状态 (Ctrl, Shift, Alt 等)
        // Byte 1: 保留字节
        // Byte 2-7: 按键扫描码

        let current_modifiers = self.parse_modifiers(report[0]);
        let previous_modifiers = self.parse_modifiers(self.previous_state[0]);

        // 检查修饰键变化
        if current_modifiers != previous_modifiers {
            // 这里可以根据需要生成修饰键事件
            debug!("Modifier change: {previous_modifiers:?} -> {current_modifiers:?}");
        }

        // 提取当前按下的键
        let current_keys: Vec<u8> = report[2..8]
            .iter()
            .filter(|&&key| key != 0)
            .cloned()
            .collect();
        let previous_keys: Vec<u8> = self.previous_state[2..8]
            .iter()
            .filter(|&&key| key != 0)
            .cloned()
            .collect();

        // 检测新按下的键
        for &scancode in &current_keys {
            if !previous_keys.contains(&scancode)
                && let Some(key) = scancode_to_key(scancode)
            {
                events.push(KeyEvent::KeyDown {
                    key,
                    modifiers: current_modifiers,
                });
            }
        }

        // 检测释放的键
        for &scancode in &previous_keys {
            if !current_keys.contains(&scancode)
                && let Some(key) = scancode_to_key(scancode)
            {
                events.push(KeyEvent::KeyUp {
                    key,
                    modifiers: previous_modifiers,
                });
            }
        }

        events
    }

    /// 解析修饰键状态
    fn parse_modifiers(&self, modifier_byte: u8) -> Modifiers {
        let mut modifiers = Modifiers::empty();

        if modifier_byte & 0x01 != 0 {
            // Left Ctrl
            modifiers |= Modifiers::CONTROL;
        }
        if modifier_byte & 0x02 != 0 {
            // Left Shift
            modifiers |= Modifiers::SHIFT;
        }
        if modifier_byte & 0x04 != 0 {
            // Left Alt
            modifiers |= Modifiers::ALT;
        }
        if modifier_byte & 0x08 != 0 {
            // Left GUI (Windows/Cmd)
            modifiers |= Modifiers::META;
        }
        if modifier_byte & 0x10 != 0 {
            // Right Ctrl
            modifiers |= Modifiers::CONTROL;
        }
        if modifier_byte & 0x20 != 0 {
            // Right Shift
            modifiers |= Modifiers::SHIFT;
        }
        if modifier_byte & 0x40 != 0 {
            // Right Alt
            modifiers |= Modifiers::ALT;
        }
        if modifier_byte & 0x80 != 0 {
            // Right GUI (Windows/Cmd)
            modifiers |= Modifiers::META;
        }

        modifiers
    }

    /// 获取当前按下的所有键
    pub fn get_pressed_keys(&self) -> Vec<Key> {
        let mut keys = Vec::new();
        for &scancode in &self.previous_state[2..8] {
            if scancode != 0
                && let Some(key) = scancode_to_key(scancode)
            {
                keys.push(key);
            }
        }
        keys
    }

    /// 获取当前修饰键状态
    pub fn get_modifiers(&self) -> Modifiers {
        self.parse_modifiers(self.previous_state[0])
    }
}
