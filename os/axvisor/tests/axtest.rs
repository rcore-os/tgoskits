#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use ax_std as _;
use axvm as _;

#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn axvisor_axtest_smoke() {
        ax_assert!(true);
    }
}
