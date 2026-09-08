//! V4L2 文件句柄与事件队列。

use alloc::{collections::VecDeque, vec::Vec};

use crate::{
    Result, V4l2Error,
    interface::event::{CtrlChange, Event, EventCtrlPayload, EventSubscription, EventType},
};

/// 订阅队列长度默认值。
pub const EVENT_QUEUE_DEFAULT_ELEMS: usize = 1;

/// 事件队列溢出时的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowStrategy {
    DropOldest,
    Ctrl,
}

/// `SubscribedEvent::push` 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    Inserted,
    Merged,
}

/// `V4l2Fh::queue_event` 的投递结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueOutcome {
    Delivered,
    Merged,
    NoSubscription,
}

/// V4L2 文件句柄。
#[derive(Debug)]
pub struct V4l2Fh {
    subscribed: Vec<SubscribedEvent>,
    sequence: u32,
    pending_count: usize,
    prio: u32,
}

impl V4l2Fh {
    /// 创建文件句柄。
    pub fn new() -> Self {
        Self {
            subscribed: Vec::new(),
            sequence: u32::MAX,
            pending_count: 0,
            prio: 2,
        }
    }

    /// 获取优先级。
    pub fn prio(&self) -> u32 {
        self.prio
    }

    /// 设置优先级。
    pub fn set_prio(&mut self, prio: u32) {
        self.prio = prio;
    }

    /// 订阅事件。
    pub fn subscribe(&mut self, sub: &EventSubscription) -> Result<()> {
        if sub.ty == EventType::All {
            return Err(V4l2Error::InvalidArgument);
        }
        if self.is_subscribed(sub.ty, sub.id) {
            return Ok(());
        }
        let (elems, strategy) = Self::subscription_params(sub.ty);
        self.subscribed.push(SubscribedEvent {
            ty: sub.ty,
            id: sub.id,
            strategy,
            elems,
            events: VecDeque::with_capacity(elems),
        });
        Ok(())
    }

    /// 取消订阅。
    pub fn unsubscribe(&mut self, sub: &EventSubscription) {
        if sub.ty == EventType::All {
            self.unsubscribe_all();
            return;
        }
        let Some(pos) = self
            .subscribed
            .iter()
            .position(|s| s.ty == sub.ty && s.id == sub.id)
        else {
            return;
        };
        let sev = self.subscribed.remove(pos);
        self.pending_count = self.pending_count.saturating_sub(sev.events.len());
        debug_assert_eq!(
            self.pending_count,
            self.subscribed.iter().map(|s| s.events.len()).sum()
        );
    }

    /// 投递事件。
    pub fn queue_event(&mut self, mut ev: Event) -> QueueOutcome {
        let Some(idx) = self
            .subscribed
            .iter()
            .position(|s| s.ty as u32 == ev.ty && s.id == ev.id)
        else {
            return QueueOutcome::NoSubscription;
        };
        ev.sequence = self.alloc_sequence();
        let sev = &mut self.subscribed[idx];
        let outcome = sev.push(ev);
        if outcome == PushOutcome::Inserted {
            self.pending_count += 1;
            QueueOutcome::Delivered
        } else {
            QueueOutcome::Merged
        }
    }

    /// 投递事件（bool 返回）。
    pub fn queue_event_bool(&mut self, ev: Event) -> bool {
        !matches!(self.queue_event(ev), QueueOutcome::NoSubscription)
    }

    /// 取出事件。
    pub fn dequeue(&mut self) -> Result<Event> {
        let idx = self
            .subscribed
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.events.front().map(|e| (i, e.sequence)))
            .min_by_key(|&(_, seq)| seq)
            .map(|(i, _)| i)
            .ok_or(V4l2Error::NoEntry)?;
        let sev = &mut self.subscribed[idx];
        let mut ev = sev.events.pop_front().expect("front selected above");
        self.pending_count = self.pending_count.saturating_sub(1);
        ev.pending = self.pending_count as u32;
        debug_assert_eq!(
            self.pending_count,
            self.subscribed.iter().map(|s| s.events.len()).sum()
        );
        Ok(ev)
    }

    /// 待处理事件数。
    pub fn pending(&self) -> usize {
        self.pending_count
    }

    /// 是否已订阅。
    pub fn is_subscribed(&self, ty: EventType, id: u32) -> bool {
        self.subscribed.iter().any(|s| s.ty == ty && s.id == id)
    }

    fn alloc_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn subscription_params(ty: EventType) -> (usize, OverflowStrategy) {
        match ty {
            EventType::Ctrl => (EVENT_QUEUE_DEFAULT_ELEMS, OverflowStrategy::Ctrl),
            _ => (EVENT_QUEUE_DEFAULT_ELEMS, OverflowStrategy::DropOldest),
        }
    }

    fn unsubscribe_all(&mut self) {
        self.pending_count = 0;
        self.subscribed.clear();
    }
}

impl Default for V4l2Fh {
    fn default() -> Self {
        Self::new()
    }
}

/// 订阅及其事件队列。
#[derive(Debug)]
struct SubscribedEvent {
    ty: EventType,
    id: u32,
    strategy: OverflowStrategy,
    elems: usize,
    events: VecDeque<Event>,
}

impl SubscribedEvent {
    /// 入队事件。
    fn push(&mut self, ev: Event) -> PushOutcome {
        if self.events.len() < self.elems {
            self.events.push_back(ev);
            return PushOutcome::Inserted;
        }
        let mut oldest = self.events.pop_front().expect("full queue is non-empty");
        if self.elems == 1 && self.strategy == OverflowStrategy::Ctrl {
            ctrl_replace(&mut oldest, &ev);
            oldest.ty = ev.ty;
            oldest.id = ev.id;
            oldest.sequence = ev.sequence;
            oldest.timestamp = ev.timestamp;
            self.events.push_back(oldest);
        } else {
            if self.elems > 1
                && self.strategy == OverflowStrategy::Ctrl
                && let Some(newest) = self.events.back_mut()
            {
                ctrl_merge(&oldest, newest);
            }
            self.events.push_back(ev);
        }
        PushOutcome::Merged
    }
}

fn ctrl_replace(old: &mut Event, new: &Event) {
    let old_changes = EventCtrlPayload::read_from(old).changes;
    let mut payload = EventCtrlPayload::read_from(new);
    payload.changes |= old_changes;
    payload.write_into(&mut old.data);
}

fn ctrl_merge(old: &Event, new: &mut Event) {
    let mut payload = EventCtrlPayload::read_from(new);
    payload.changes |= EventCtrlPayload::read_from(old).changes;
    payload.write_into(&mut new.data);
}

/// 构建控件事件参数。
#[derive(Debug, Clone, Copy)]
pub struct CtrlEventParams {
    pub id: u32,
    pub ctrl_type: u32,
    pub value: i64,
    pub flags: u32,
    pub minimum: i64,
    pub maximum: i64,
    pub step: i64,
    pub default_value: i64,
}

/// 构建 CTRL 事件。
pub fn build_ctrl_event(params: CtrlEventParams, changes: CtrlChange) -> Event {
    let ctrl = EventCtrlPayload {
        changes: changes.bits(),
        ty: params.ctrl_type,
        value: params.value as u64,
        flags: params.flags,
        minimum: params.minimum as i32,
        maximum: params.maximum as i32,
        step: params.step as i32,
        default_value: params.default_value as i32,
    };
    let mut data = [0u8; 64];
    ctrl.write_into(&mut data);
    Event {
        ty: EventType::Ctrl as u32,
        pad: 0,
        data,
        pending: 0,
        sequence: 0,
        timestamp: crate::interface::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        id: params.id,
        reserved: [0; 8],
    }
}
