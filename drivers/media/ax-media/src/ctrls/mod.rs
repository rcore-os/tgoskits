//! V4L2 控件框架。

pub mod class;
mod define;

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicI64, Ordering};

use crate::{
    Result, V4l2Error,
    filehandler::{CtrlEventParams, V4l2Fh, build_ctrl_event},
    interface::{
        ctrl::{
            CID_PRIVATE_BASE, CTRL_ID_MASK, CTRL_MAX_DIMS, CTRL_WHICH_DEF_VAL, CTRL_WHICH_MAX_VAL,
            CTRL_WHICH_MIN_VAL, CTRL_WHICH_REQUEST_VAL, Control, CtrlFlags, ExtControl,
            ExtControls, QueryCtrl, QueryExtCtrl, Querymenu,
        },
        event::{CtrlChange, Event, EventSubFlags, EventSubscription, EventType},
    },
};

const CTRL_NEXT_CTRL: u32 = 0x8000_0000;
const CTRL_NEXT_COMPOUND: u32 = 0x4000_0000;

// ── 控件类型 ─────────────────────────────────────────────────────────

/// V4L2 控件类型
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlType {
    Integer   = 1,
    Boolean   = 2,
    Menu      = 3,
    Button    = 4,
    Integer64 = 5,
    CtrlClass = 6,
    Bitmask   = 8,
}

impl CtrlType {
    /// 转换控件类型。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Integer,
            2 => Self::Boolean,
            3 => Self::Menu,
            4 => Self::Button,
            5 => Self::Integer64,
            6 => Self::CtrlClass,
            8 => Self::Bitmask,
            _ => return None,
        })
    }

    pub fn is_int(&self) -> bool {
        *self != CtrlType::Integer64
    }

    pub fn size(&self) -> u32 {
        if *self == CtrlType::Integer64 { 8 } else { 4 }
    }

    pub fn check_range(&self, min: i64, max: i64, step: u64, def: i64) -> Result<()> {
        match self {
            CtrlType::Boolean => {
                if step != 1 || max > 1 || min < 0 {
                    return Err(V4l2Error::OutOfRange);
                }
                if step == 0 || min > max || def < min || def > max {
                    return Err(V4l2Error::OutOfRange);
                }
                Ok(())
            }
            CtrlType::Integer | CtrlType::Integer64 => {
                if step == 0 || min > max || def < min || def > max {
                    return Err(V4l2Error::OutOfRange);
                }
                Ok(())
            }
            CtrlType::Bitmask => {
                if step != 0 || min != 0 || max == 0 || (def & !max) != 0 {
                    return Err(V4l2Error::OutOfRange);
                }
                Ok(())
            }
            CtrlType::Menu => {
                if min > max || def < min || def > max || min < 0 || (step != 0 && max >= 64) {
                    return Err(V4l2Error::OutOfRange);
                }
                if def < 64 && (step & (1u64 << def)) != 0 {
                    return Err(V4l2Error::InvalidArgument);
                }
                Ok(())
            }
            CtrlType::Button | CtrlType::CtrlClass => Ok(()),
        }
    }
}

// ── 控件回调 ─────────────────────

pub type CtrlGetFn = Box<dyn Fn() -> Result<i64> + Send + Sync>;
pub type CtrlTryFn = Box<dyn Fn(i64) -> Result<i64> + Send + Sync>;
pub type CtrlSetFn = Box<dyn Fn(i64) -> Result<i64> + Send + Sync>;

/// 控件硬件回调集合。
pub struct CtrlOps {
    pub get: Option<CtrlGetFn>,
    pub try_ctrl: Option<CtrlTryFn>,
    pub set: CtrlSetFn,
}

// ── 控件配置 ───────────────────

/// 注册控件所需的完整配置。
pub struct CtrlConfig {
    pub id: u32,
    pub name: &'static str,
    pub ctrl_type: CtrlType,
    pub minimum: i64,
    pub maximum: i64,
    pub step: u64,
    pub default_value: i64,
    pub flags: CtrlFlags,
    pub qmenu: Option<&'static [&'static str]>,
    pub ops: Option<CtrlOps>,
}

// ── 控件 ─────────────────────────────────────────────────────────────

/// 已注册的控件，包含元数据与当前值。
pub struct Ctrl {
    pub id: u32,
    pub name: &'static str,
    pub ctrl_type: CtrlType,
    pub minimum: i64,
    pub maximum: i64,
    pub step: u64,
    pub default_value: i64,
    pub flags: CtrlFlags,
    pub qmenu: Option<&'static [&'static str]>,
    pub(crate) ops: Option<CtrlOps>,
    cur: AtomicI64,
}

impl Ctrl {
    /// 当前值（无锁读取，供驱动填充 / 采样路径使用）。
    pub fn value(&self) -> i64 {
        self.cur.load(Ordering::Acquire)
    }

    fn set_value(&self, v: i64) {
        self.cur.store(v, Ordering::Release);
    }
}

/// 值变化通知回调：控件值在 `S_CTRL` / `S_EXT_CTRLS` 中改变时触发，
/// 载荷为完整 `V4L2_EVENT_CTRL` 事件。驱动用它把事件推入共享事件队列。
pub type CtrlChangeNotify = Box<dyn Fn(Event) + Send + Sync>;

// ── 控件处理器 ─────────────────────────────────────────────────────

/// 控件处理器。
pub struct CtrlHandler {
    pub(crate) ctrls: Vec<Ctrl>,
    notify: Option<CtrlChangeNotify>,
}

impl CtrlHandler {
    /// 创建空处理器。
    pub fn new() -> Self {
        Self {
            ctrls: Vec::new(),
            notify: None,
        }
    }

    /// 设置值变化通知回调。
    pub fn set_change_notify(&mut self, notify: CtrlChangeNotify) {
        self.notify = Some(notify);
    }

    // ── 查找 ───────────────────────────────────────────────────────

    /// 按 ID 查找控件。
    pub fn find(&self, id: u32) -> Option<&Ctrl> {
        let id = id & CTRL_ID_MASK;
        self.ctrls
            .binary_search_by_key(&id, |c| c.id)
            .ok()
            .map(|i| &self.ctrls[i])
    }

    /// 按 ID 读取当前值。
    pub fn value(&self, id: u32) -> Option<i64> {
        self.find(id).map(Ctrl::value)
    }

    /// 设置指定控件的 step 掩码（用于菜单跳过掩码等特殊初始化）。
    pub fn set_step(&mut self, id: u32, step: u64) {
        if let Some(ctrl) = self.ctrls.iter_mut().find(|c| c.id == (id & CTRL_ID_MASK)) {
            ctrl.step = step;
        }
    }

    /// 已注册控件的数量。
    pub fn len(&self) -> usize {
        self.ctrls.len()
    }

    /// 是否没有注册任何控件。
    pub fn is_empty(&self) -> bool {
        self.ctrls.is_empty()
    }

    /// 遍历所有控件的迭代器（按 id 升序；同时实现 `ExactSizeIterator` /
    /// `DoubleEndedIterator`）。
    pub fn iter(&self) -> core::slice::Iter<'_, Ctrl> {
        self.ctrls.iter()
    }

    /// 枚举下一个控件。
    fn next_ctrl(&self, id: u32, next_compound_only: bool, next_all: bool) -> Option<&Ctrl> {
        let pos = self.ctrls.partition_point(|c| c.id <= id);
        self.ctrls[pos..]
            .iter()
            .find(|_| next_ctrl_match(next_compound_only, next_all))
    }

    // ── 查询 IOCTL ────────────────────────────────────────────────

    /// 填充事件载荷。
    fn fill_event(&self, ctrl: &Ctrl, changes: CtrlChange) -> Option<Event> {
        Some(build_ctrl_event(
            CtrlEventParams {
                id: ctrl.id,
                ctrl_type: ctrl.ctrl_type as u32,
                value: ctrl.value(),
                flags: ctrl.flags.bits(),
                minimum: ctrl.minimum,
                maximum: ctrl.maximum,
                step: ctrl.step as i64,
                default_value: ctrl.default_value,
            },
            changes,
        ))
    }

    /// 构建值变化事件。
    pub fn change_event(&self, id: u32, changes: CtrlChange) -> Option<Event> {
        let ctrl = self.find(id)?;
        self.fill_event(ctrl, changes)
    }

    /// 触发值变化通知。
    fn emit_change(&self, ctrl: &Ctrl, changes: CtrlChange) {
        if let Some(notify) = &self.notify
            && let Some(ev) = self.fill_event(ctrl, changes)
        {
            notify(ev);
        }
    }

    // ── 内部辅助 ───────────────────────────────────────────────────

    /// 校验单个扩展控件。
    fn prepare_ext_ctrl(&self, which: u32, c: &ExtControl) -> Result<&Ctrl> {
        let id = unsafe { core::ptr::addr_of!(c.id).read_unaligned() } & CTRL_ID_MASK;
        let which_in_range = (CTRL_WHICH_DEF_VAL..=CTRL_WHICH_MAX_VAL).contains(&which);
        if which != 0 && !which_in_range && id2which(id) != which {
            return Err(V4l2Error::InvalidArgument);
        }
        // 旧式私有控件不允许用于扩展控件。
        if id >= CID_PRIVATE_BASE {
            return Err(V4l2Error::InvalidArgument);
        }
        let ctrl = self.find(id).ok_or(V4l2Error::InvalidArgument)?;
        if ctrl.flags.contains(CtrlFlags::DISABLED) {
            return Err(V4l2Error::InvalidArgument);
        }
        if !ctrl.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX)
            && (which == CTRL_WHICH_MIN_VAL || which == CTRL_WHICH_MAX_VAL)
        {
            return Err(V4l2Error::InvalidArgument);
        }
        Ok(ctrl)
    }

    /// 类检查。
    fn class_check(&self, which: u32) -> Result<()> {
        if which == 0 || (CTRL_WHICH_DEF_VAL..=CTRL_WHICH_MAX_VAL).contains(&which) {
            return Ok(());
        }
        // 检查是否存在属于该类的控件。
        let which_class = id2which(which);
        if self.ctrls.iter().any(|c| id2which(c.id) == which_class) {
            Ok(())
        } else {
            Err(V4l2Error::InvalidArgument)
        }
    }

    /// 读取易变控件值。
    fn read_volatile(&self, ctrl: &Ctrl) -> Result<i64> {
        if let Some(ops) = &ctrl.ops
            && let Some(get) = &ops.get
        {
            get()
        } else {
            Ok(ctrl.value())
        }
    }

    /// 校验并取整。
    fn validate_new(&self, ctrl: &Ctrl, v: i64) -> Result<i64> {
        let validated = match ctrl.ctrl_type {
            CtrlType::Integer | CtrlType::Integer64 => round_to_range(v, ctrl),
            CtrlType::Boolean => {
                if v != 0 {
                    1
                } else {
                    0
                }
            }
            CtrlType::Menu => {
                if v < ctrl.minimum || v > ctrl.maximum {
                    return Err(V4l2Error::OutOfRange);
                }
                if v < 64 && (ctrl.step & (1u64 << v)) != 0 {
                    return Err(V4l2Error::InvalidArgument);
                }
                if let Some(qmenu) = ctrl.qmenu {
                    let name = qmenu.get(v as usize).ok_or(V4l2Error::InvalidArgument)?;
                    if name.is_empty() {
                        return Err(V4l2Error::InvalidArgument);
                    }
                }
                v
            }
            CtrlType::Bitmask => v & ctrl.maximum,
            CtrlType::Button | CtrlType::CtrlClass => 0,
        };
        if let Some(ops) = &ctrl.ops
            && let Some(try_fn) = &ops.try_ctrl
        {
            return try_fn(validated);
        }
        Ok(validated)
    }

    /// 应用值。
    fn apply_value(&self, ctrl: &Ctrl, v: i64) -> Result<i64> {
        let execute = ctrl.flags.contains(CtrlFlags::EXECUTE_ON_WRITE);
        // VOLATILE 控件每次写入设备。
        let always_write = execute || ctrl.flags.contains(CtrlFlags::VOLATILE);
        let cur = ctrl.value();
        if !always_write && cur == v {
            return Ok(v);
        }
        // `set` 为硬件控件必填回调；内存控件（`ops == None`）直接写 `cur`。
        let new = if let Some(ops) = &ctrl.ops {
            (ops.set)(v)?
        } else {
            v
        };
        if new != cur || execute {
            ctrl.set_value(new);
            self.emit_change(ctrl, CtrlChange::VALUE);
        }
        Ok(new)
    }
}

impl CtrlHandler {
    pub fn query_ext_ctrl(&self, q: &mut QueryExtCtrl) -> Result<()> {
        let next_flags = CTRL_NEXT_CTRL | CTRL_NEXT_COMPOUND;
        let id = q.id & CTRL_ID_MASK;
        let enum_next = (q.id & next_flags) != 0 && !self.ctrls.is_empty();
        let next_compound_only = (q.id & next_flags) == CTRL_NEXT_COMPOUND;
        let next_all = (q.id & next_flags) == next_flags;

        let ctrl = if enum_next {
            self.next_ctrl(id, next_compound_only, next_all)
        } else {
            self.find(id)
        }
        .ok_or(V4l2Error::InvalidArgument)?;

        q.id = if id >= CID_PRIVATE_BASE { id } else { ctrl.id };
        q.ty = ctrl.ctrl_type as u32;
        let name = ctrl.name.as_bytes();
        let len = name.len().min(q.name.len() - 1);
        q.name[..len].copy_from_slice(&name[..len]);
        q.name[len] = 0;
        q.flags = ctrl.flags;
        q.elem_size = ctrl.ctrl_type.size();
        q.elems = 1;
        q.nr_of_dims = 0;
        q.dims = [0; CTRL_MAX_DIMS as usize];
        q.minimum = ctrl.minimum;
        q.maximum = ctrl.maximum;
        q.default_value = ctrl.default_value;
        q.step = if ctrl.ctrl_type == CtrlType::Menu {
            1
        } else {
            ctrl.step
        };
        q.reserved = [0; 32];
        Ok(())
    }

    pub fn queryctrl(&self, q: &mut QueryCtrl) -> Result<()> {
        let mut qec = QueryExtCtrl {
            id: q.id,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            elem_size: 0,
            elems: 0,
            nr_of_dims: 0,
            dims: [0; CTRL_MAX_DIMS as usize],
            reserved: [0; 32],
        };
        self.query_ext_ctrl(&mut qec)?;

        // v4l2_query_ext_ctrl_to_v4l2_queryctrl：仅标量兼容类型拷贝范围。
        q.id = qec.id;
        q.ty = qec.ty;
        q.name = qec.name;
        q.flags = qec.flags;
        q.reserved = [0; 2];
        match CtrlType::try_from_u32(qec.ty) {
            Some(CtrlType::Integer | CtrlType::Boolean | CtrlType::Menu | CtrlType::Bitmask) => {
                q.minimum = qec.minimum as i32;
                q.maximum = qec.maximum as i32;
                q.step = qec.step as i32;
                q.default_value = qec.default_value as i32;
            }
            _ => {
                q.minimum = 0;
                q.maximum = 0;
                q.step = 0;
                q.default_value = 0;
            }
        }
        Ok(())
    }

    pub fn querymenu(&self, q: &mut Querymenu) -> Result<()> {
        let ctrl = self.find(q.id).ok_or(V4l2Error::InvalidArgument)?;
        if ctrl.ctrl_type != CtrlType::Menu {
            return Err(V4l2Error::InvalidArgument);
        }
        let qmenu = ctrl.qmenu.ok_or(V4l2Error::InvalidArgument)?;
        let i = q.index;
        if i < ctrl.minimum as u32 || i > ctrl.maximum as u32 {
            return Err(V4l2Error::InvalidArgument);
        }
        // 跳过掩码。
        if i < 64 && (ctrl.step & (1u64 << i)) != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let name = qmenu.get(i as usize).ok_or(V4l2Error::InvalidArgument)?;
        if name.is_empty() {
            return Err(V4l2Error::InvalidArgument);
        }
        let b = name.as_bytes();
        let len = b.len().min(q.name.len() - 1);
        q.name[..len].copy_from_slice(&b[..len]);
        q.name[len] = 0;
        q.reserved = 0;
        Ok(())
    }

    pub fn g_ext_ctrls(&self, h: &mut ExtControls, cs: &mut [ExtControl]) -> Result<()> {
        let is_default = h.which == CTRL_WHICH_DEF_VAL;
        let is_request = h.which == CTRL_WHICH_REQUEST_VAL;
        let is_min = h.which == CTRL_WHICH_MIN_VAL;
        let is_max = h.which == CTRL_WHICH_MAX_VAL;

        h.error_idx = h.count;
        h.which = id2which(h.which);

        if is_request {
            return Err(V4l2Error::NotSupported);
        }
        if h.count == 0 {
            return self.class_check(h.which);
        }

        // 准备阶段：逐项校验。
        let mut refs = Vec::with_capacity(cs.len());
        for (i, c) in cs.iter().enumerate() {
            h.error_idx = i as u32;
            let ctrl = match self.prepare_ext_ctrl(h.which, c) {
                Ok(ctrl) => ctrl,
                Err(e) => {
                    h.error_idx = h.count;
                    return Err(e);
                }
            };
            refs.push(ctrl);
        }
        h.error_idx = h.count;

        // WRITE_ONLY 控件不可读。
        if refs.iter().any(|c| c.flags.contains(CtrlFlags::WRITE_ONLY)) {
            return Err(V4l2Error::AccessDenied);
        }

        for (c, ctrl) in cs.iter_mut().zip(refs) {
            let v = if is_default {
                ctrl.default_value
            } else if is_min {
                ctrl.minimum
            } else if is_max {
                ctrl.maximum
            } else if ctrl.flags.contains(CtrlFlags::VOLATILE) {
                self.read_volatile(ctrl)?
            } else {
                ctrl.value()
            };
            write_ext_value(c, ctrl.ctrl_type, v);
        }
        h.error_idx = h.count;
        Ok(())
    }

    pub fn try_ext_ctrls(&self, h: &mut ExtControls, cs: &mut [ExtControl]) -> Result<()> {
        self.try_set_ext_ctrls(h, cs, false)
    }

    pub fn s_ext_ctrls(&self, h: &mut ExtControls, cs: &mut [ExtControl]) -> Result<()> {
        self.try_set_ext_ctrls(h, cs, true)
    }

    fn try_set_ext_ctrls(
        &self,
        h: &mut ExtControls,
        cs: &mut [ExtControl],
        set: bool,
    ) -> Result<()> {
        h.error_idx = h.count;

        // 默认 / 最小 / 最大值不可修改。
        if matches!(
            h.which,
            CTRL_WHICH_DEF_VAL | CTRL_WHICH_MIN_VAL | CTRL_WHICH_MAX_VAL
        ) {
            return Err(V4l2Error::InvalidArgument);
        }
        h.which = id2which(h.which);
        if h.which == CTRL_WHICH_REQUEST_VAL {
            return Err(V4l2Error::NotSupported);
        }
        if h.count == 0 {
            return self.class_check(h.which);
        }

        // 准备 + 校验。
        let mut resolved: Vec<(&Ctrl, i64)> = Vec::with_capacity(cs.len());
        for (i, c) in cs.iter().enumerate() {
            h.error_idx = i as u32;
            let ctrl = match self.prepare_ext_ctrl(h.which, c) {
                Ok(ctrl) => ctrl,
                Err(e) => {
                    if set {
                        h.error_idx = h.count;
                    }
                    return Err(e);
                }
            };
            if ctrl.flags.contains(CtrlFlags::READ_ONLY) {
                if set {
                    h.error_idx = h.count;
                }
                return Err(V4l2Error::AccessDenied);
            }
            if set && ctrl.flags.contains(CtrlFlags::GRABBED) {
                h.error_idx = h.count;
                return Err(V4l2Error::Busy);
            }
            let v = read_ext_value(c, ctrl.ctrl_type);
            let target = match self.validate_new(ctrl, v) {
                Ok(t) => t,
                Err(e) => {
                    if set {
                        h.error_idx = h.count;
                    }
                    return Err(e);
                }
            };
            resolved.push((ctrl, target));
        }

        // 回写校验后的值。
        for (c, &(ctrl, target)) in cs.iter_mut().zip(&resolved) {
            write_ext_value(c, ctrl.ctrl_type, target);
        }

        // 应用阶段（仅 S）：调用 try/s_ctrl 回调并更新当前值。
        if set {
            for (c, &(ctrl, target)) in cs.iter_mut().zip(&resolved) {
                let new = self.apply_value(ctrl, target)?;
                write_ext_value(c, ctrl.ctrl_type, new);
            }
        }

        h.error_idx = h.count;
        Ok(())
    }

    // ── 兼容 G/S_CTRL ─────────

    pub fn g_ctrl(&self, c: &mut Control) -> Result<()> {
        let ctrl = self.find(c.id).ok_or(V4l2Error::InvalidArgument)?;
        if !ctrl.ctrl_type.is_int() {
            return Err(V4l2Error::InvalidArgument);
        }
        if ctrl.flags.contains(CtrlFlags::WRITE_ONLY) {
            return Err(V4l2Error::AccessDenied);
        }
        let v = if ctrl.flags.contains(CtrlFlags::VOLATILE) {
            self.read_volatile(ctrl)?
        } else {
            ctrl.value()
        };
        c.value = v as i32;
        Ok(())
    }

    pub fn s_ctrl(&self, c: &mut Control) -> Result<()> {
        let ctrl = self.find(c.id).ok_or(V4l2Error::InvalidArgument)?;
        if !ctrl.ctrl_type.is_int() {
            return Err(V4l2Error::InvalidArgument);
        }
        if ctrl.flags.contains(CtrlFlags::READ_ONLY) {
            return Err(V4l2Error::AccessDenied);
        }
        let target = self.validate_new(ctrl, c.value as i64)?;
        let new = self.apply_value(ctrl, target)?;
        c.value = new as i32;
        Ok(())
    }

    pub fn subscribe_event(&self, fh: &mut V4l2Fh, sub: &EventSubscription) -> Result<()> {
        if sub.ty != EventType::Ctrl {
            return Err(V4l2Error::InvalidArgument);
        }
        let ctrl = self.find(sub.id).ok_or(V4l2Error::InvalidArgument)?;
        // 已订阅：幂等返回，不重复发送初始事件。
        if fh.is_subscribed(sub.ty, sub.id) {
            return Ok(());
        }
        fh.subscribe(sub)?;
        if sub.flags.contains(EventSubFlags::SEND_INITIAL) {
            // 控制类（CtrlClass）不产生初始事件，对齐 v4l2-compliance 对 User Controls 的期望
            if ctrl.ctrl_type == CtrlType::CtrlClass {
                return Ok(());
            }
            let changes = if ctrl.flags.contains(CtrlFlags::WRITE_ONLY) {
                CtrlChange::FLAGS
            } else {
                CtrlChange::VALUE | CtrlChange::FLAGS
            };
            if let Some(ev) = self.fill_event(ctrl, changes) {
                fh.queue_event(ev);
            }
        }
        Ok(())
    }
}

impl Default for CtrlHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// 通过引用迭代全部控件（`for c in &handler`），按 id 升序。
///
/// 迭代器为 `slice::Iter`（实现 `ExactSizeIterator` / `DoubleEndedIterator`）。
/// 控件元数据只读，故不提供 `&mut Ctrl` 迭代。
impl<'a> IntoIterator for &'a CtrlHandler {
    type Item = &'a Ctrl;
    type IntoIter = core::slice::Iter<'a, Ctrl>;

    fn into_iter(self) -> Self::IntoIter {
        self.ctrls.iter()
    }
}

// ── 模块级辅助 ─────────────────────────────────────────────────────

/// 控件 ID 转类。
fn id2which(id: u32) -> u32 {
    id & 0x0fff_0000
}

/// NEXT_CTRL 过滤。
fn next_ctrl_match(next_compound_only: bool, next_all: bool) -> bool {
    next_all || !next_compound_only
}

/// 读取控件值。
fn read_ext_value(c: &ExtControl, ty: CtrlType) -> i64 {
    if ty == CtrlType::Integer64 {
        unsafe { core::ptr::addr_of!(c.value.value64).read_unaligned() }
    } else {
        unsafe { core::ptr::addr_of!(c.value.value).read_unaligned() as i64 }
    }
}

/// 写回控件值。
fn write_ext_value(c: &mut ExtControl, ty: CtrlType, v: i64) {
    if ty == CtrlType::Integer64 {
        unsafe { core::ptr::addr_of_mut!(c.value.value64).write_unaligned(v) }
    } else {
        unsafe { core::ptr::addr_of_mut!(c.value.value).write_unaligned(v as i32) }
    }
}

/// 整数取整。
fn round_to_range(v: i64, ctrl: &Ctrl) -> i64 {
    let step = ctrl.step as i64;
    let half = step / 2;
    let v = if ctrl.maximum >= 0 && v >= ctrl.maximum - half {
        ctrl.maximum
    } else {
        v + half
    };
    let v = v.clamp(ctrl.minimum, ctrl.maximum);
    let offset = v - ctrl.minimum;
    let offset = step * (offset / step);
    ctrl.minimum + offset
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::{
        ctrls::{CtrlConfig, CtrlOps},
        interface::{
            ctrl::{ExtControlValue, QueryCtrl},
            event::{EventCtrlPayload, EventSubFlags},
        },
    };

    const NEXT_CTRL: u32 = CTRL_NEXT_CTRL;
    const BRIGHTNESS: u32 = 0x0098_0900;
    const CONTRAST: u32 = 0x0098_0901;
    const TEST_PATTERN: u32 = 0x0098_0930;

    const TEST_PATTERN_MENU: &[&str] = &[
        "75% Colorbar",
        "100% Colorbar",
        "CSC Colorbar",
        "Black",
        "White",
    ];

    fn register_uvc_like(handler: &mut CtrlHandler) {
        // 乱序注册（对齐 UVC_CONTROL_DEFS 顺序：非 id 升序）。
        handler
            .new_int(0x0098_091C, "Backlight", 0, 255, 1, 0, None)
            .unwrap();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 0, None)
            .unwrap();
        handler
            .new_int(CONTRAST, "Contrast", 0, 255, 1, 0, None)
            .unwrap();
        handler
            .new_int(0x009A_0901, "ExposureAuto", 0, 4, 1, 0, None)
            .unwrap();
        handler
            .new_int(0x009A_0902, "ExposureAbs", 0, 10_000, 1, 0, None)
            .unwrap();
    }

    fn zero_query_ctrl() -> QueryCtrl {
        QueryCtrl {
            id: 0,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            reserved: [0; 2],
        }
    }

    fn ext_ctrl(id: u32, value: i32) -> ExtControl {
        ExtControl {
            id,
            size: 0,
            reserved2: [0; 1],
            value: ExtControlValue { value },
        }
    }

    fn ext_ctrl_i64(id: u32, value: i64) -> ExtControl {
        ExtControl {
            id,
            size: 0,
            reserved2: [0; 1],
            value: ExtControlValue { value64: value },
        }
    }

    fn ext_header(count: u32, which: u32) -> ExtControls {
        ExtControls {
            which,
            count,
            error_idx: 0,
            request_fd: 0,
            reserved: [0; 1],
            controls: 0,
        }
    }

    fn read_value(c: &ExtControl) -> i32 {
        // SAFETY: 测试中构造的控件均为非 Integer64 标量，读取 value 成员。
        unsafe { c.value.value }
    }

    /// NEXT_CTRL 枚举必须严格递增、返回 id 不得携带 NEXT 标志、
    /// 枚举完必须 EINVAL 终止。
    #[test]
    fn next_ctrl_enumeration_is_strictly_increasing_and_terminates() {
        let mut handler = CtrlHandler::new();
        register_uvc_like(&mut handler);

        let mut q = QueryExtCtrl {
            id: NEXT_CTRL,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            elem_size: 0,
            elems: 0,
            nr_of_dims: 0,
            dims: [0; 4],
            reserved: [0; 32],
        };
        let mut last_id = 0u32;
        let mut count = 0u32;
        loop {
            match handler.query_ext_ctrl(&mut q) {
                Ok(()) => {
                    assert_eq!(q.id & NEXT_CTRL, 0, "returned id carries NEXT flag");
                    assert!(q.id > last_id, "id not strictly increasing");
                    last_id = q.id;
                    count += 1;
                    q.id = last_id | NEXT_CTRL;
                    assert!(count <= 16, "enumeration did not terminate");
                }
                Err(V4l2Error::InvalidArgument) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(
            count, 5,
            "should enumerate exactly the 5 registered controls"
        );
    }

    /// QUERYCTRL 由 QUERY_EXT_CTRL 换算：id/name/flags 与范围字段正确。
    #[test]
    fn queryctrl_derives_from_query_ext_ctrl() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let mut q = zero_query_ctrl();
        q.id = BRIGHTNESS;
        handler.queryctrl(&mut q).unwrap();
        assert_eq!(q.id, BRIGHTNESS);
        assert_eq!(q.ty, CtrlType::Integer as u32);
        assert_eq!(&q.name[..10], b"Brightness");
        assert_eq!(q.minimum, 0);
        assert_eq!(q.maximum, 255);
        assert_eq!(q.step, 1);
        assert_eq!(q.default_value, 128);
        assert!(q.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX));
    }

    /// QUERYMENU：越界索引 / 非菜单控件返回 EINVAL，合法项返回名称。
    #[test]
    fn querymenu_resolves_menu_items() {
        let mut handler = CtrlHandler::new();
        handler
            .new_menu(TEST_PATTERN, "Test Pattern", 5, 0, TEST_PATTERN_MENU, None)
            .unwrap();

        let mut q = Querymenu {
            id: TEST_PATTERN,
            index: 0,
            name: [0; 32],
            reserved: 0,
        };
        handler.querymenu(&mut q).unwrap();
        assert_eq!(&q.name[..12], b"75% Colorbar");

        q.index = 3;
        handler.querymenu(&mut q).unwrap();
        assert_eq!(&q.name[..5], b"Black");

        q.index = 99;
        assert!(matches!(
            handler.querymenu(&mut q),
            Err(V4l2Error::InvalidArgument)
        ));
    }

    /// G_EXT_CTRLS 读取与 error_idx。
    #[test]
    fn g_ext_ctrls_reads_values_and_sets_error_idx() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 0)];
        let mut h = ext_header(1, 0);
        handler.g_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 128);
        assert_eq!(h.error_idx, 1, "success -> error_idx == count");

        let mut cs = [ext_ctrl(0xDEAD_BEEF, 0)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.g_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(
            h.error_idx, 1,
            "g_ext_ctrls validation failure -> error_idx == count"
        );

        // TRY/S 对相同非法 ID 的区分：TRY 留失败索引，S 置 count
        let mut cs = [ext_ctrl(0xDEAD_BEEF, 0)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.try_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(
            h.error_idx, 0,
            "try_ext_ctrls validation failure -> failing index"
        );
        let mut cs = [ext_ctrl(0xDEAD_BEEF, 0)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.s_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(h.error_idx, 1, "s_ext_ctrls validation failure -> count");
    }

    #[test]
    fn ext_ctrls_rejects_write_only_and_read_only() {
        let mut h1 = CtrlHandler::new();
        h1.new_ctrl(CtrlConfig {
            id: 0x0098_0931,
            name: "Action",
            ctrl_type: CtrlType::Button,
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            qmenu: None,
            ops: None,
        })
        .unwrap();
        let mut cs = [ext_ctrl(0x0098_0931, 0)];
        let mut hdr = ext_header(1, 0);
        assert!(matches!(
            h1.g_ext_ctrls(&mut hdr, &mut cs),
            Err(V4l2Error::AccessDenied)
        ));

        let mut h2 = CtrlHandler::new();
        h2.new_ctrl(CtrlConfig {
            id: BRIGHTNESS,
            name: "Readonly",
            ctrl_type: CtrlType::Integer,
            minimum: 0,
            maximum: 255,
            step: 1,
            default_value: 0,
            flags: CtrlFlags::READ_ONLY,
            qmenu: None,
            ops: None,
        })
        .unwrap();
        let mut cs = [ext_ctrl(BRIGHTNESS, 10)];
        let mut hdr = ext_header(1, 0);
        assert!(matches!(
            h2.s_ext_ctrls(&mut hdr, &mut cs),
            Err(V4l2Error::AccessDenied)
        ));
    }

    /// G/TRY/S_EXT_CTRLS mixed-class 校验：G/S 要求 error_idx == count，TRY 要求 < count
    /// （对齐 v4l2-compliance 1073 / v4l2-test-controls.cpp mixed-class 分支）。
    #[test]
    fn ext_ctrls_mixed_class_error_idx() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap(); // class 0x00980000
        handler
            .new_int(0x009A_0901, "ExposureAuto", 0, 3, 1, 0, None)
            .unwrap(); // class 0x009a0000
        let which = 0x0098_0000;
        let mut cs = [ext_ctrl(BRIGHTNESS, 0), ext_ctrl(0x009A_0901, 0)];

        let mut h = ext_header(2, which);
        assert!(matches!(
            handler.g_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(h.error_idx, 2, "G mixed-class -> count");

        let mut cs = [ext_ctrl(BRIGHTNESS, 0), ext_ctrl(0x009A_0901, 0)];
        let mut h = ext_header(2, which);
        assert!(matches!(
            handler.try_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(h.error_idx, 1, "TRY mixed-class -> failing index");
        assert!(h.error_idx < h.count);

        let mut cs = [ext_ctrl(BRIGHTNESS, 0), ext_ctrl(0x009A_0901, 0)];
        let mut h = ext_header(2, which);
        assert!(matches!(
            handler.s_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(h.error_idx, 2, "S mixed-class -> count");
    }

    /// S_EXT_CTRLS：整数按步长取整；越界 clamp 到 [min, max]。
    #[test]
    fn s_ext_ctrls_rounds_to_step() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 100, 10, 0, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 55)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 60);
        assert_eq!(handler.value(BRIGHTNESS), Some(60));

        let mut cs = [ext_ctrl(BRIGHTNESS, 1000)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 100);
    }

    /// S_EXT_CTRLS：菜单越界返回 EINVAL 且不改变任何控件。
    #[test]
    fn s_ext_ctrls_rejects_bad_menu_value() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        handler
            .new_menu(TEST_PATTERN, "Test Pattern", 5, 0, TEST_PATTERN_MENU, None)
            .unwrap();

        let mut cs = [ext_ctrl(TEST_PATTERN, 99)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.s_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::OutOfRange)
        ));
        assert_eq!(handler.value(TEST_PATTERN), Some(0));
    }

    /// TRY_EXT_CTRLS：回写校验后的值，但不应用（不改变当前值）。
    #[test]
    fn try_ext_ctrls_validates_without_applying() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 100, 10, 0, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 55)];
        let mut h = ext_header(1, 0);
        handler.try_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 60, "try 回写校验后的值");
        assert_eq!(handler.value(BRIGHTNESS), Some(0), "try 不应用");
    }

    /// S_EXT_CTRLS：值变化触发 change notify 回调（事件载荷）。
    #[test]
    fn s_ext_ctrls_fires_notify_on_change() {
        use alloc::sync::Arc;
        use core::sync::atomic::{AtomicU32, AtomicUsize};

        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let last_id = Arc::new(AtomicU32::new(0));
        let cnt = Arc::clone(&count);
        let id = Arc::clone(&last_id);
        handler.set_change_notify(Box::new(move |ev| {
            cnt.fetch_add(1, Ordering::Relaxed);
            id.store(ev.id, Ordering::Relaxed);
        }));

        let mut cs = [ext_ctrl(BRIGHTNESS, 200)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(last_id.load(Ordering::Relaxed), BRIGHTNESS);

        // 未变化：不触发。
        let mut cs = [ext_ctrl(BRIGHTNESS, 200)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    /// INTEGER64 控件走 value64。
    #[test]
    fn integer64_uses_value64() {
        let mut handler = CtrlHandler::new();
        handler
            .new_ctrl(CtrlConfig {
                id: 0x0098_0903,
                name: "Tstamp",
                ctrl_type: CtrlType::Integer64,
                minimum: 0,
                maximum: 1_000_000,
                step: 1,
                default_value: 42,
                flags: CtrlFlags::empty(),
                qmenu: None,
                ops: None,
            })
            .unwrap();

        let mut cs = [ext_ctrl_i64(0x0098_0903, 0)];
        let mut h = ext_header(1, 0);
        handler.g_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(unsafe { cs[0].value.value64 }, 42);

        let mut cs = [ext_ctrl_i64(0x0098_0903, 500)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(handler.value(0x0098_0903), Some(500));
    }

    // ── 控件事件 ────────────────────────────────────────────────────

    fn ctrl_sub(id: u32, flags: EventSubFlags) -> EventSubscription {
        EventSubscription {
            ty: EventType::Ctrl,
            id,
            flags,
            reserved: [0; 5],
        }
    }

    fn read_ctrl(ev: &Event) -> EventCtrlPayload {
        let mut payload = [0u8; core::mem::size_of::<EventCtrlPayload>()];
        payload.copy_from_slice(&ev.data[..core::mem::size_of::<EventCtrlPayload>()]);
        // SAFETY: EventCtrlPayload 是 repr(C) POD，长度等于 size_of。
        unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const EventCtrlPayload) }
    }

    /// SEND_INITIAL：订阅后立即投递初始事件。
    #[test]
    fn subscribe_with_send_initial_queues_initial_event() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        let mut fh = V4l2Fh::new();

        handler
            .subscribe_event(&mut fh, &ctrl_sub(BRIGHTNESS, EventSubFlags::SEND_INITIAL))
            .unwrap();
        assert_eq!(fh.pending(), 1, "SEND_INITIAL queues one initial event");

        let out = fh.dequeue().unwrap();
        assert_eq!(out.ty, EventType::Ctrl as u32);
        assert_eq!(out.id, BRIGHTNESS);
        assert_eq!(out.reserved, [0; 8], "reserved must be zeroed");
        let payload = read_ctrl(&out);
        assert_eq!(
            payload.changes,
            (CtrlChange::VALUE | CtrlChange::FLAGS).bits(),
            "initial event changes = VALUE|FLAGS"
        );
        assert_eq!(payload.value, 128, "initial event carries current value");
    }

    /// 订阅不存在的控件 ID 或非 CTRL 类型必须 EINVAL。
    #[test]
    fn subscribe_rejects_unknown_ctrl_and_non_ctrl_type() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        let mut fh = V4l2Fh::new();

        assert!(matches!(
            handler.subscribe_event(&mut fh, &ctrl_sub(0xDEAD_BEEF, EventSubFlags::empty())),
            Err(V4l2Error::InvalidArgument)
        ));
        assert!(matches!(
            handler.subscribe_event(
                &mut fh,
                &EventSubscription {
                    ty: EventType::Eos,
                    id: BRIGHTNESS,
                    flags: EventSubFlags::empty(),
                    reserved: [0; 5],
                }
            ),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(fh.pending(), 0);
    }

    #[test]
    fn hardware_proxy_ctrl_uses_ops() {
        use alloc::sync::Arc;
        use core::sync::atomic::AtomicUsize;

        let device_val = Arc::new(AtomicI64::new(5));
        let set_calls = Arc::new(AtomicUsize::new(0));

        let get_dev = Arc::clone(&device_val);
        let set_dev = Arc::clone(&device_val);
        let set_cnt = Arc::clone(&set_calls);
        let ops = CtrlOps {
            get: Some(Box::new(move || Ok(get_dev.load(Ordering::Relaxed)))),
            try_ctrl: None,
            set: Box::new(move |v| {
                set_cnt.fetch_add(1, Ordering::Relaxed);
                set_dev.store(v.clamp(0, 255), Ordering::Relaxed);
                Ok(set_dev.load(Ordering::Relaxed))
            }),
        };
        let mut handler = CtrlHandler::new();
        handler
            .new_ctrl(CtrlConfig {
                id: BRIGHTNESS,
                name: "Brightness",
                ctrl_type: CtrlType::Integer,
                minimum: 0,
                maximum: 255,
                step: 1,
                default_value: 0,
                flags: CtrlFlags::VOLATILE,
                qmenu: None,
                ops: Some(ops),
            })
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 200)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 200);
        assert_eq!(set_calls.load(Ordering::Relaxed), 1);

        let mut cs = [ext_ctrl(BRIGHTNESS, 0)];
        let mut h = ext_header(1, 0);
        handler.g_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 200, "VOLATILE 控件 G 读取设备值");
    }
}
