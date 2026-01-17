#![no_main]
#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(not(any(windows, unix)))]

use core::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use log::info;
use sparreal_rt::os::time::{one_shot_after, one_shot_at, since_boot, time_list};

extern crate alloc;
#[macro_use]
extern crate sparreal_rt;

// ============================================================================
// 测试辅助宏和函数
// ============================================================================

macro_rules! assert_test {
    ($cond:expr, $msg:expr) => {
        if !$cond {
            panic!("Test failed: {}", $msg);
        }
    };
}

fn wait_for_flag(flag: &AtomicBool, timeout_ms: u64) -> bool {
    let start = since_boot();
    let timeout = Duration::from_millis(timeout_ms);
    loop {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        if since_boot().saturating_sub(start) > timeout {
            return false;
        }
    }
}

fn wait_for_count(counter: &AtomicUsize, expected: usize, timeout_ms: u64) -> bool {
    let start = since_boot();
    let timeout = Duration::from_millis(timeout_ms);
    loop {
        if counter.load(Ordering::SeqCst) >= expected {
            return true;
        }
        if since_boot().saturating_sub(start) > timeout {
            return false;
        }
    }
}

// ============================================================================
// 测试用例
// ============================================================================

/// 测试1: 基本的 one_shot_after - 单次定时器触发
fn test_one_shot_after_basic() {
    info!("[TEST] test_one_shot_after_basic");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);

    one_shot_after(Duration::from_millis(100), || {
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    assert_test!(
        wait_for_flag(&TRIGGERED, 500),
        "one_shot_after callback not triggered"
    );

    info!("[PASS] test_one_shot_after_basic");
}

/// 测试2: one_shot_at - 在指定时间点触发
fn test_one_shot_at_basic() {
    info!("[TEST] test_one_shot_at_basic");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);

    let deadline = since_boot() + Duration::from_millis(100);
    one_shot_at(deadline, || {
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    assert_test!(
        wait_for_flag(&TRIGGERED, 500),
        "one_shot_at callback not triggered"
    );

    info!("[PASS] test_one_shot_at_basic");
}

/// 测试3: 多个定时器按顺序触发
fn test_multiple_timers_order() {
    info!("[TEST] test_multiple_timers_order");

    static ORDER: AtomicUsize = AtomicUsize::new(0);
    static FIRST: AtomicBool = AtomicBool::new(false);
    static SECOND: AtomicBool = AtomicBool::new(false);
    static THIRD: AtomicBool = AtomicBool::new(false);

    // 注册顺序: 300ms, 100ms, 200ms
    // 期望触发顺序: 100ms, 200ms, 300ms
    one_shot_after(Duration::from_millis(300), || {
        let order = ORDER.fetch_add(1, Ordering::SeqCst);
        assert_test!(order == 2, "Third timer should fire last");
        THIRD.store(true, Ordering::SeqCst);
    })
    .unwrap();

    one_shot_after(Duration::from_millis(100), || {
        let order = ORDER.fetch_add(1, Ordering::SeqCst);
        assert_test!(order == 0, "First timer should fire first");
        FIRST.store(true, Ordering::SeqCst);
    })
    .unwrap();

    one_shot_after(Duration::from_millis(200), || {
        let order = ORDER.fetch_add(1, Ordering::SeqCst);
        assert_test!(order == 1, "Second timer should fire second");
        SECOND.store(true, Ordering::SeqCst);
    })
    .unwrap();

    assert_test!(wait_for_flag(&FIRST, 500), "First timer not triggered");
    assert_test!(wait_for_flag(&SECOND, 500), "Second timer not triggered");
    assert_test!(wait_for_flag(&THIRD, 500), "Third timer not triggered");

    info!("[PASS] test_multiple_timers_order");
}

/// 测试4: 取消定时器
fn test_cancel_timer() {
    info!("[TEST] test_cancel_timer");

    static SHOULD_NOT_TRIGGER: AtomicBool = AtomicBool::new(false);
    static SHOULD_TRIGGER: AtomicBool = AtomicBool::new(false);

    let handle = one_shot_after(Duration::from_millis(100), || {
        SHOULD_NOT_TRIGGER.store(true, Ordering::SeqCst);
    })
    .unwrap();

    one_shot_after(Duration::from_millis(200), || {
        SHOULD_TRIGGER.store(true, Ordering::SeqCst);
    })
    .unwrap();

    // 取消第一个定时器
    let cancelled = handle.cancel();
    assert_test!(cancelled, "Timer should be cancelled successfully");

    // 等待足够时间
    assert_test!(
        wait_for_flag(&SHOULD_TRIGGER, 500),
        "Second timer should trigger"
    );

    // 第一个定时器不应该触发
    assert_test!(
        !SHOULD_NOT_TRIGGER.load(Ordering::SeqCst),
        "Cancelled timer should not trigger"
    );

    info!("[PASS] test_cancel_timer");
}

/// 测试5: 取消不存在的定时器
fn test_cancel_invalid_timer() {
    info!("[TEST] test_cancel_invalid_timer");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);

    let handle = one_shot_after(Duration::from_millis(50), || {
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    // 等待定时器触发
    assert_test!(wait_for_flag(&TRIGGERED, 500), "Timer should trigger");

    // 定时器已经触发，再次取消应该返回 false
    let cancelled = handle.cancel();
    assert_test!(!cancelled, "Already fired timer should not be cancellable");

    info!("[PASS] test_cancel_invalid_timer");
}

/// 测试6: 极短延迟定时器
fn test_short_delay() {
    info!("[TEST] test_short_delay");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);

    // 非常短的延迟 (1ms)
    one_shot_after(Duration::from_millis(1), || {
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    assert_test!(
        wait_for_flag(&TRIGGERED, 500),
        "Short delay timer not triggered"
    );

    info!("[PASS] test_short_delay");
}

/// 测试7: 零延迟定时器（应该立即或很快触发）
fn test_zero_delay() {
    info!("[TEST] test_zero_delay");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);

    one_shot_after(Duration::ZERO, || {
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    assert_test!(
        wait_for_flag(&TRIGGERED, 500),
        "Zero delay timer not triggered"
    );

    info!("[PASS] test_zero_delay");
}

/// 测试8: 定时器精度测试
fn test_timer_accuracy() {
    info!("[TEST] test_timer_accuracy");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);
    static TRIGGER_TIME: AtomicUsize = AtomicUsize::new(0);

    let delay = Duration::from_millis(100);
    let start = since_boot();

    one_shot_after(delay, move || {
        let elapsed = since_boot().saturating_sub(start);
        TRIGGER_TIME.store(elapsed.as_millis() as usize, Ordering::SeqCst);
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    assert_test!(wait_for_flag(&TRIGGERED, 500), "Timer not triggered");

    let actual_delay_ms = TRIGGER_TIME.load(Ordering::SeqCst);
    // 允许 ±50ms 的误差
    assert_test!(
        (50..=200).contains(&actual_delay_ms),
        "Timer accuracy out of range"
    );

    info!("[PASS] test_timer_accuracy (expected: ~100ms, actual: {actual_delay_ms}ms)");
}

/// 测试9: time_list 检查待处理定时器
fn test_time_list() {
    info!("[TEST] test_time_list");

    static TRIGGERED: AtomicBool = AtomicBool::new(false);

    // 添加一个较长延迟的定时器
    let _handle = one_shot_after(Duration::from_millis(500), || {
        TRIGGERED.store(true, Ordering::SeqCst);
    })
    .unwrap();

    // 检查 time_list
    let list = time_list();
    assert_test!(
        !list.is_empty(),
        "time_list should contain at least one timer"
    );

    info!("[INFO] Pending timers: {}", list.len());
    for entry in &list {
        info!(
            "  - handle: {:?}, deadline: {:?}, remaining: {:?}",
            entry.handle, entry.deadline, entry.remaining
        );
    }

    // 等待定时器触发
    assert_test!(wait_for_flag(&TRIGGERED, 1000), "Timer not triggered");

    // 触发后列表应该少一个
    let list_after = time_list();
    info!("[INFO] Pending timers after trigger: {}", list_after.len());

    info!("[PASS] test_time_list");
}

/// 测试10: since_boot 单调递增
fn test_since_boot_monotonic() {
    info!("[TEST] test_since_boot_monotonic");

    let mut prev = since_boot();
    for _ in 0..100 {
        let now = since_boot();
        assert_test!(now >= prev, "since_boot should be monotonically increasing");
        prev = now;
    }

    info!("[PASS] test_since_boot_monotonic");
}

/// 测试11: 多个同时到期的定时器
fn test_concurrent_deadlines() {
    info!("[TEST] test_concurrent_deadlines");

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    let deadline = since_boot() + Duration::from_millis(100);

    // 多个定时器设置相同的截止时间
    for _ in 0..5 {
        one_shot_at(deadline, || {
            COUNTER.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    }

    assert_test!(
        wait_for_count(&COUNTER, 5, 500),
        "Not all concurrent timers triggered"
    );

    info!("[PASS] test_concurrent_deadlines");
}

// ============================================================================
// 主函数
// ============================================================================

#[sparreal_rt::entry]
fn main() {
    info!("========================================");
    info!("Timer Test Suite");
    info!("========================================");

    test_since_boot_monotonic();
    test_one_shot_after_basic();
    test_one_shot_at_basic();
    test_short_delay();
    test_zero_delay();
    test_timer_accuracy();
    test_multiple_timers_order();
    test_cancel_timer();
    test_cancel_invalid_timer();
    test_time_list();
    test_concurrent_deadlines();

    info!("========================================");
    println!("All tests passed!");
    info!("========================================");
}
