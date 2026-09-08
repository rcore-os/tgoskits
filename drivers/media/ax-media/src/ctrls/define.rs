use core::sync::atomic::AtomicI64;

use crate::{
    Result, V4l2Error,
    ctrls::{Ctrl, CtrlConfig, CtrlHandler, CtrlOps, CtrlType},
    interface::ctrl::{CID_PRIVATE_BASE, CtrlFlags},
};

impl CtrlHandler {
    /// 有序插入。
    fn insert_sorted(&mut self, ctrl: Ctrl) -> Result<()> {
        let id = ctrl.id;
        match self.ctrls.binary_search_by_key(&id, |c| c.id) {
            Ok(_) => Err(V4l2Error::InvalidArgument),
            Err(pos) => {
                self.ctrls.insert(pos, ctrl);
                Ok(())
            }
        }
    }

    /// 注册控件。
    pub fn new_ctrl(&mut self, cfg: CtrlConfig) -> Result<()> {
        if cfg.id == 0 || cfg.name.is_empty() || cfg.id >= CID_PRIVATE_BASE {
            return Err(V4l2Error::OutOfRange);
        }
        if cfg.ctrl_type == CtrlType::Menu && cfg.qmenu.is_none() {
            return Err(V4l2Error::OutOfRange);
        }
        cfg.ctrl_type
            .check_range(cfg.minimum, cfg.maximum, cfg.step, cfg.default_value)?;
        if cfg.ctrl_type == CtrlType::Menu
            && let Some(qmenu) = cfg.qmenu
            && cfg.maximum >= 0
            && cfg.maximum as usize >= qmenu.len()
        {
            return Err(V4l2Error::OutOfRange);
        }

        // 设置类型相关标志。
        let mut flags = cfg.flags;
        if !matches!(cfg.ctrl_type, CtrlType::Button | CtrlType::CtrlClass) {
            flags |= CtrlFlags::HAS_WHICH_MIN_MAX;
        }
        match cfg.ctrl_type {
            CtrlType::Button => {
                flags |= CtrlFlags::WRITE_ONLY | CtrlFlags::EXECUTE_ON_WRITE;
            }
            CtrlType::CtrlClass => flags |= CtrlFlags::READ_ONLY | CtrlFlags::WRITE_ONLY,
            _ => {}
        }

        let ctrl = Ctrl {
            id: cfg.id,
            name: cfg.name,
            ctrl_type: cfg.ctrl_type,
            minimum: cfg.minimum,
            maximum: cfg.maximum,
            step: cfg.step,
            default_value: cfg.default_value,
            flags,
            qmenu: cfg.qmenu,
            ops: cfg.ops,
            cur: AtomicI64::new(cfg.default_value),
        };
        self.insert_sorted(ctrl)
    }

    /// 注册整数控件。
    #[allow(clippy::too_many_arguments)]
    pub fn new_int(
        &mut self,
        id: u32,
        name: &'static str,
        min: i64,
        max: i64,
        step: i64,
        default: i64,
        ops: Option<CtrlOps>,
    ) -> Result<()> {
        let flags = if ops.is_some() {
            CtrlFlags::VOLATILE
        } else {
            CtrlFlags::empty()
        };
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Integer,
            minimum: min,
            maximum: max,
            step: step as u64,
            default_value: default,
            flags,
            qmenu: None,
            ops,
        })
    }

    /// 注册布尔控件。
    pub fn new_bool(
        &mut self,
        id: u32,
        name: &'static str,
        default: bool,
        ops: Option<CtrlOps>,
    ) -> Result<()> {
        let flags = if ops.is_some() {
            CtrlFlags::VOLATILE
        } else {
            CtrlFlags::empty()
        };
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Boolean,
            minimum: 0,
            maximum: 1,
            step: 1,
            default_value: default as i64,
            flags,
            qmenu: None,
            ops,
        })
    }

    /// 注册菜单控件。
    pub fn new_menu(
        &mut self,
        id: u32,
        name: &'static str,
        items: u32,
        default: u32,
        qmenu: &'static [&'static str],
        ops: Option<CtrlOps>,
    ) -> Result<()> {
        let flags = if ops.is_some() {
            CtrlFlags::VOLATILE
        } else {
            CtrlFlags::empty()
        };
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Menu,
            minimum: 0,
            maximum: items as i64 - 1,
            step: 0,
            default_value: default as i64,
            flags,
            qmenu: Some(qmenu),
            ops,
        })
    }

    /// 注册按钮控件。
    pub fn new_button(&mut self, id: u32, name: &'static str, ops: Option<CtrlOps>) -> Result<()> {
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Button,
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            qmenu: None,
            ops,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{V4l2Error, ctrls::CtrlType, interface::ctrl::CtrlFlags};

    const BRIGHTNESS: u32 = 0x0098_0900;

    #[test]
    fn new_ctrl_rejects_duplicate_id() {
        let mut h = CtrlHandler::new();
        h.new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 0, None)
            .unwrap();
        let err = h
            .new_int(BRIGHTNESS, "Brightness2", 0, 255, 1, 0, None)
            .unwrap_err();
        assert!(matches!(err, V4l2Error::InvalidArgument));
    }

    #[test]
    fn new_ctrl_validates_range_and_flags() {
        let mut h = CtrlHandler::new();
        // min > max
        assert!(h.new_int(BRIGHTNESS, "Bad", 100, 0, 1, 0, None).is_err());
        // Menu without qmenu
        assert!(
            h.new_ctrl(CtrlConfig {
                id: BRIGHTNESS,
                name: "NoMenu",
                ctrl_type: CtrlType::Menu,
                minimum: 0,
                maximum: 1,
                step: 0,
                default_value: 0,
                flags: CtrlFlags::empty(),
                qmenu: None,
                ops: None,
            })
            .is_err()
        );
        let menu = &["A", "B", "C"];
        // default out of range / qmenu len < items
        assert!(h.new_menu(0x0098_0901, "Bad", 3, 5, menu, None).is_err());
        assert!(h.new_menu(0x0098_0902, "Bad2", 5, 0, menu, None).is_err());
        h.new_menu(0x0098_0900, "Menu", 3, 2, menu, None).unwrap();
        // VOLATILE auto-append for HW proxy
        let mut hw = CtrlHandler::new();
        let ops = crate::ctrls::CtrlOps {
            get: Some(Box::new(|| Ok(0))),
            try_ctrl: None,
            set: Box::new(|v| Ok(v)),
        };
        hw.new_int(BRIGHTNESS, "HW", 0, 255, 1, 0, Some(ops))
            .unwrap();
        assert!(
            hw.find(BRIGHTNESS)
                .unwrap()
                .flags
                .contains(CtrlFlags::VOLATILE)
        );
        let mut sw = CtrlHandler::new();
        sw.new_int(BRIGHTNESS, "SW", 0, 255, 1, 0, None).unwrap();
        assert!(
            !sw.find(BRIGHTNESS)
                .unwrap()
                .flags
                .contains(CtrlFlags::VOLATILE)
        );
        // Button forces WRITE_ONLY|EXECUTE, no HAS_WHICH, 0 range ok
        let mut hb = CtrlHandler::new();
        hb.new_button(0x0098_0931, "Btn", None).unwrap();
        let ctrl = hb.find(0x0098_0931).unwrap();
        assert!(ctrl.flags.contains(CtrlFlags::WRITE_ONLY));
        assert!(ctrl.flags.contains(CtrlFlags::EXECUTE_ON_WRITE));
        assert!(!ctrl.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX));
        assert_eq!(ctrl.ctrl_type, CtrlType::Button);
        let mut h2 = CtrlHandler::new();
        h2.new_button(0x0098_0931, "Btn", None).unwrap();
        assert_eq!(h2.len(), 1);
    }
}
