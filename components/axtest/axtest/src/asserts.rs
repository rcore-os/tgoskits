#[macro_export]
macro_rules! ax_assert {
    ($cond:expr $(,)?) => {
        if !$cond {
            return $crate::AxTestResult::Failed;
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            let _ = core::format_args!($($arg)+);
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
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
    ($left:expr, $right:expr, $($arg:tt)+) => {
        match (&$left, &$right) {
            (left_val, right_val) => {
                if !(*left_val == *right_val) {
                    let _ = core::format_args!($($arg)+);
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
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
    ($left:expr, $right:expr, $($arg:tt)+) => {
        match (&$left, &$right) {
            (left_val, right_val) => {
                if *left_val == *right_val {
                    let _ = core::format_args!($($arg)+);
                    return $crate::AxTestResult::Failed;
                }
            }
        }
    };
}
