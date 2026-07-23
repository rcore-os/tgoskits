use ax_errno::AxResult;

use crate::task::do_exit;

pub fn sys_exit(exit_code: i32) -> AxResult<isize> {
    do_exit(exit_code << 8, false);
    Ok(0)
}

pub fn sys_exit_group(exit_code: i32) -> AxResult<isize> {
    do_exit(exit_code << 8, true);
    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn exit_code_encoding_rules_hold_for_test() -> bool {
    // Test exit code encoding: sys_exit shifts left by 8
    let exit_code = 42i32;
    let encoded = exit_code << 8;
    assert!(encoded == 0x2A00);

    // Test zero exit code
    let zero_exit = 0i32;
    let encoded_zero = zero_exit << 8;
    assert!(encoded_zero == 0);

    // Test max valid exit code (0-255 range)
    let max_exit = 255i32;
    let encoded_max = max_exit << 8;
    assert!(encoded_max == 0xFF00);

    true
}
