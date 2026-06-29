#[macro_export]
macro_rules! ax_assert {
    ($cond:expr $(,)?) => {
        if !$cond {
            return $crate::AxTestResult::Failed;
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            $crate::axtest_println!(
                "assertion failed: {}",
                core::format_args!($($arg)+)
            );
            return $crate::AxTestResult::Failed;
        }
    };
}

#[macro_export]
macro_rules! ax_assert_eq {
    ($left:expr, $right:expr $(,)?) => {
        match (&$left, &$right) {
            (left_val, right_val) => {
                if !(*left_val == *right_val) {
                    $crate::axtest_println!(
                        "assertion `left == right` failed\n  left: {left_val:?}\n right: {right_val:?}"
                    );
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
    ($left:expr, $right:expr, $($arg:tt)+) => {
        match (&$left, &$right) {
            (left_val, right_val) => {
                if !(*left_val == *right_val) {
                    $crate::axtest_println!(
                        "assertion `left == right` failed: {}\n  left: {left_val:?}\n right: {right_val:?}",
                        core::format_args!($($arg)+)
                    );
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
}

#[macro_export]
macro_rules! ax_assert_ne {
    ($left:expr, $right:expr $(,)?) => {
        match (&$left, &$right) {
            (left_val, right_val) => {
                if *left_val == *right_val {
                    $crate::axtest_println!(
                        "assertion `left != right` failed\n  left: {left_val:?}\n right: {right_val:?}"
                    );
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
    ($left:expr, $right:expr, $($arg:tt)+) => {
        match (&$left, &$right) {
            (left_val, right_val) => {
                if *left_val == *right_val {
                    $crate::axtest_println!(
                        "assertion `left != right` failed: {}\n  left: {left_val:?}\n right: {right_val:?}",
                        core::format_args!($($arg)+)
                    );
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
}
