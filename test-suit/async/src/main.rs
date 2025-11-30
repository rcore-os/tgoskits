//! Async Executor Test Suite for no_std environment
//!
//! 这个测试套件用于验证异步执行器的核心功能，包括：
//! - 任务生成与执行
//! - 多任务调度
//! - block_on 同步等待
//! - 执行器状态管理
//! - 定时器集成

#![no_std]
#![no_main]

extern crate alloc;
#[macro_use]
extern crate sparreal_rt;

use core::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use log::info;
use sparreal_kernel::os::r#async::{
    SingleCpuExecutor, block_on, has_pending_tasks, spawn, task_count, tick,
};
use sparreal_rt::os::time::{one_shot_after, since_boot};

// ============================================================================
// 测试辅助工具
// ============================================================================

/// 断言宏，失败时 panic
macro_rules! assert_test {
    ($cond:expr, $msg:expr) => {
        if !$cond {
            panic!("Test failed: {}", $msg);
        }
    };
}

/// 带超时的异步任务调度
fn run_until_idle(timeout_ms: u64) {
    let start = since_boot();
    let timeout = Duration::from_millis(timeout_ms);

    while has_pending_tasks() {
        tick();
        if since_boot().saturating_sub(start) > timeout {
            info!("[ASYNC] Scheduler timeout");
            break;
        }
        for _ in 0..100 {
            core::hint::spin_loop();
        }
    }
}

// ============================================================================
// 测试用例
// ============================================================================

/// 测试: 执行器初始状态
fn test_executor_state() {
    info!("[TEST] test_executor_state");

    let executor = SingleCpuExecutor::new();
    assert_test!(
        executor.task_count() == 0,
        "New executor should have 0 tasks"
    );
    assert_test!(
        !executor.has_pending_tasks(),
        "New executor should have no pending tasks"
    );
    assert_test!(!executor.is_running(), "New executor should not be running");

    info!("[PASS] test_executor_state");
}

/// 测试: 基本任务生成和执行
fn test_basic_spawn_and_run() {
    info!("[TEST] test_basic_spawn_and_run");

    static EXECUTED: AtomicBool = AtomicBool::new(false);

    spawn(async {
        EXECUTED.store(true, Ordering::SeqCst);
        info!("[ASYNC] Basic task executed");
    });

    assert_test!(task_count() == 1, "Task count should be 1");
    run_until_idle(1000);
    assert_test!(EXECUTED.load(Ordering::SeqCst), "Task should have executed");
    assert_test!(task_count() == 0, "Task count should be 0");

    info!("[PASS] test_basic_spawn_and_run");
}

/// 测试: block_on 同步等待
fn test_block_on() {
    info!("[TEST] test_block_on");

    static EXECUTED: AtomicBool = AtomicBool::new(false);

    block_on(async {
        EXECUTED.store(true, Ordering::SeqCst);
        info!("[ASYNC] Block-on task executed");
    });

    assert_test!(
        EXECUTED.load(Ordering::SeqCst),
        "Block-on task should have executed"
    );
    assert_test!(task_count() == 0, "No tasks should remain");

    info!("[PASS] test_block_on");
}

/// 测试: 多任务并发执行
fn test_multiple_tasks() {
    info!("[TEST] test_multiple_tasks");

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    const TASK_COUNT: usize = 5;

    for i in 0..TASK_COUNT {
        spawn(async move {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            info!("[ASYNC] Task {i} executed");
        });
    }

    assert_test!(task_count() == TASK_COUNT, "Should have 5 tasks");
    run_until_idle(2000);
    assert_test!(
        COUNTER.load(Ordering::SeqCst) == TASK_COUNT,
        "All tasks should have executed"
    );
    assert_test!(task_count() == 0, "All tasks should be cleaned up");

    info!("[PASS] test_multiple_tasks");
}

/// 测试: 任务执行顺序 (FIFO)
fn test_task_order() {
    info!("[TEST] test_task_order");

    static ORDER: AtomicUsize = AtomicUsize::new(0);
    static FIRST_ORDER: AtomicUsize = AtomicUsize::new(usize::MAX);
    static SECOND_ORDER: AtomicUsize = AtomicUsize::new(usize::MAX);

    spawn(async {
        FIRST_ORDER.store(ORDER.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
        info!("[ASYNC] First task executed");
    });

    spawn(async {
        SECOND_ORDER.store(ORDER.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
        info!("[ASYNC] Second task executed");
    });

    run_until_idle(1000);

    assert_test!(
        FIRST_ORDER.load(Ordering::SeqCst) == 0,
        "First task should execute first"
    );
    assert_test!(
        SECOND_ORDER.load(Ordering::SeqCst) == 1,
        "Second task should execute second"
    );

    info!("[PASS] test_task_order");
}

/// 测试: 复杂异步操作（循环内多次操作）
fn test_complex_operations() {
    info!("[TEST] test_complex_operations");

    static COMPLETED: AtomicBool = AtomicBool::new(false);
    static OP_COUNT: AtomicUsize = AtomicUsize::new(0);

    spawn(async {
        info!("[ASYNC] Complex task started");
        for i in 0..3 {
            OP_COUNT.fetch_add(1, Ordering::SeqCst);
            info!("[ASYNC] Operation {i}");
        }
        COMPLETED.store(true, Ordering::SeqCst);
        info!("[ASYNC] Complex task completed");
    });

    run_until_idle(1500);

    assert_test!(
        COMPLETED.load(Ordering::SeqCst),
        "Task should have completed"
    );
    assert_test!(
        OP_COUNT.load(Ordering::SeqCst) == 3,
        "All operations should have executed"
    );

    info!("[PASS] test_complex_operations");
}

/// 测试: 执行器压力测试
fn test_stress() {
    info!("[TEST] test_stress");

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    const TASK_COUNT: usize = 20;

    for i in 0..TASK_COUNT {
        spawn(async move {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            if i % 5 == 0 {
                info!("[ASYNC] Stress task {i} executed");
            }
        });
    }

    assert_test!(task_count() == TASK_COUNT, "Should have 20 tasks");

    let start = since_boot();
    let timeout = Duration::from_millis(2000);
    while has_pending_tasks() && since_boot().saturating_sub(start) < timeout {
        tick();
    }

    assert_test!(
        COUNTER.load(Ordering::SeqCst) == TASK_COUNT,
        "All tasks should have executed"
    );
    assert_test!(task_count() == 0, "All tasks should be cleaned up");

    info!("[PASS] test_stress");
}

/// 测试: 定时器与异步集成
fn test_timer_integration() {
    info!("[TEST] test_timer_integration");

    static COMPLETED: AtomicBool = AtomicBool::new(false);

    let _handle = one_shot_after(Duration::from_millis(100), || {
        spawn(async {
            info!("[ASYNC] Timer-triggered task");
            COMPLETED.store(true, Ordering::SeqCst);
        });
    })
    .unwrap();

    let start = since_boot();
    let timeout = Duration::from_millis(500);
    while since_boot().saturating_sub(start) < timeout {
        tick();
        if COMPLETED.load(Ordering::SeqCst) {
            break;
        }
    }

    assert_test!(
        COMPLETED.load(Ordering::SeqCst),
        "Timer task should have completed"
    );

    info!("[PASS] test_timer_integration");
}

// ============================================================================
// 主函数
// ============================================================================

#[sparreal_rt::entry]
fn main() {
    info!("========================================");
    info!("Async Executor Test Suite");
    info!("========================================");

    // 执行器状态测试
    test_executor_state();

    // 基本功能测试
    test_basic_spawn_and_run();
    test_block_on();

    // 多任务测试
    test_multiple_tasks();
    test_task_order();
    test_complex_operations();

    // 压力测试
    test_stress();

    // 集成测试
    test_timer_integration();

    info!("========================================");
    println!("All async tests passed!");
    info!("========================================");
}
