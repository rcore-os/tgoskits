#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;

mod pin;

#[bare_test::tests]
mod tests {
    use bare_test::mem::iomap;
    use log::info;
    use rockchip_soc::{Cru, SocType};
    use spin::{Mutex, Once};

    use crate::pin::test_pin;

    static INIT: Once<Mutex<Cru>> = Once::new();

    pub fn initclk(clk: Cru) {
        INIT.call_once(|| Mutex::new(clk));
    }

    #[test]
    fn it_works() {
        let cru3588 = 0xfd7c0000usize;
        // let sys_grf = Cru::grf_mmio_ls()[0];
        let sys_grf_base = 0xfd58c000usize;
        let sys_grf_size = 0x1000usize;

        let base = iomap(cru3588.into(), 0x5c000);
        let sys_grf = iomap(sys_grf_base.into(), sys_grf_size);

        let cru = Cru::new(SocType::Rk3588, base, sys_grf);
        initclk(cru);

        test_pin();
    }
}
