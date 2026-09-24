//! RK3588 GPIO control for the task-switch benchmark.
//!
//! The benchmark drives GPIO3_C6 low in the peer task and high in the main
//! task, so every timed switch interval is visible on an oscilloscope. The pin
//! is written through direct memory-mapped I/O because the timed interval must
//! not take a driver lock or call a sleeping interface.
//!
//! Physical device windows are reached through `ax_mm::iomap`. The AxVisor
//! board guest is configured as a passthrough guest (see
//! `test-suit/axvisor/normal/board-orangepi-5-plus/rust-shyper-bencher`), which
//! is the same device access model the verified ArceOS task-switch benchmark
//! guest uses on this board.
//!
//! The `init` entry point maps every window the pin needs before the benchmark
//! starts and reports a failure instead of degrading into pin writes that go
//! nowhere, so the caller can stop the run when the signal is unavailable.

// The RK3588 device registers only exist on AArch64. Other targets keep the
// call sites of this module compilable, but have no pin to drive.
#[cfg(target_arch = "aarch64")]
mod rk3588 {
    use core::{
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use ax_memory_addr::PhysAddr;
    use rockchip_soc::{
        GPIO3_C6, GpioDirection, Iomux, PinConfig, PinCtrl, PinCtrlOp, Pull, SocType,
    };

    /// Physical base address of the RK3588 I/O controller (pin mux) block.
    const BUS_IOC_BASE: usize = 0xfd5f_0000;
    const BUS_IOC_SIZE: usize = 0x1_0000;

    /// Physical base addresses of the five RK3588 GPIO controllers; GPIO3 is
    /// the fourth entry.
    const GPIO_BANK_SIZE: usize = 0x100;
    const GPIO_BANK_BASES: [usize; 5] = [
        0xfd8a_0000,
        0xfec2_0000,
        0xfec3_0000,
        0xfec4_0000,
        0xfec5_0000,
    ];

    /// GPIO port data registers; the upper half of each pin's bank is addressed
    /// through the high register.
    const SWPORT_DR_L: usize = 0x00;
    const SWPORT_DR_H: usize = 0x04;
    /// GPIO external port data register.
    const EXT_PORT: usize = 0x70;
    /// GPIO controller version register.
    const VER_ID: usize = 0x78;

    /// GPIO3_C6 inside its bank: the benchmark's oscilloscope signal.
    const GPIO3_C6_IN_BANK: u32 = 22;
    /// GPIO3_B2 inside its bank: the red status LED.
    const GPIO3_B2_IN_BANK: u32 = 10;
    /// GPIO3_C0 inside its bank: the green status LED.
    const GPIO3_C0_IN_BANK: u32 = 16;

    /// Virtual address of the mapped GPIO3 register window.
    ///
    /// `init` is the only writer: the main thread maps and configures the pin
    /// before the benchmark starts, and nothing else publishes this value.
    static GPIO3_WINDOW: AtomicUsize = AtomicUsize::new(0);

    /// Maps every GPIO window the benchmark needs and configures GPIO3_C6 as a
    /// GPIO output.
    ///
    /// This must run before the benchmark starts. Returning `Err` means the
    /// oscilloscope pin cannot be driven, so the caller has to stop instead of
    /// measuring and reporting a result that no physical signal backs.
    pub fn init() -> Result<(), &'static str> {
        let mut gpio = [NonNull::dangling(); 5];
        for (window, base) in gpio.iter_mut().zip(GPIO_BANK_BASES) {
            *window = map_mmio(base, GPIO_BANK_SIZE).ok_or("gpio-bank-map-failed")?;
        }
        let ioc = map_mmio(BUS_IOC_BASE, BUS_IOC_SIZE).ok_or("bus-ioc-map-failed")?;

        let mut pinctrl = PinCtrl::new(SocType::Rk3588, ioc, &gpio);
        pinctrl
            .set_config(PinConfig {
                id: GPIO3_C6,
                mux: Iomux::empty(),
                pull: Pull::Disabled,
                drive: None,
            })
            .map_err(|_| "gpio3-c6-pinmux-config-failed")?;
        pinctrl
            .set_gpio_direction(GPIO3_C6, GpioDirection::Output(false))
            .map_err(|_| "gpio3-c6-output-config-failed")?;

        GPIO3_WINDOW.store(gpio[3].as_ptr() as usize, Ordering::Release);
        Ok(())
    }

    /// Drives GPIO3_C6 high: the benchmark's main task owns the interval.
    pub fn gpio3_output_high() {
        set_gpio_bit(GPIO3_C6_IN_BANK, true);
    }

    /// Drives GPIO3_C6 low: the benchmark's peer task owns the interval.
    pub fn gpio3_output_low() {
        set_gpio_bit(GPIO3_C6_IN_BANK, false);
    }

    /// Clears every GPIO3 output, matching the original bring-up sequence.
    pub fn gpio3_clear_all() {
        write_gpio_pair(window(), 0);
    }

    /// Turns the red status LED on.
    pub fn gpio3_led_red_on() {
        set_gpio_bit(GPIO3_B2_IN_BANK, true);
    }

    /// Turns the green status LED on.
    pub fn gpio3_led_green_on() {
        set_gpio_bit(GPIO3_C0_IN_BANK, true);
    }

    /// Prints the GPIO3 controller version register.
    pub fn gpio3_ver_id_get() {
        println!("Read GPIO_VER_ID={:#x}", mmio_read32(window() + VER_ID));
    }

    /// Prints the live level of every GPIO3 pin.
    pub fn gpio3_ext_port_signals_get() {
        println!("Read GPIO_EXT_PORT={:#x}", mmio_read32(window() + EXT_PORT));
    }

    /// Maps one physical MMIO window into the guest address space.
    fn map_mmio(base: usize, size: usize) -> Option<NonNull<u8>> {
        let addr = ax_mm::iomap(PhysAddr::from_usize(base), size).ok()?;
        NonNull::new(addr.as_mut_ptr())
    }

    /// Returns the GPIO3 register window published by `init`.
    ///
    /// `init` is the only writer and the benchmark cannot start before it
    /// succeeds, so the setter paths never fall back to a silent no-op write.
    #[inline]
    fn window() -> usize {
        GPIO3_WINDOW.load(Ordering::Relaxed)
    }

    /// Sets or clears one GPIO3 pin while preserving the other pins.
    #[inline]
    fn set_gpio_bit(pin: u32, value: bool) {
        let base = window();
        let mut current = read_gpio_pair(base);
        if value {
            current |= 1 << pin;
        } else {
            current &= !(1 << pin);
        }
        write_gpio_pair(base, current);
    }

    #[inline]
    fn read_gpio_pair(base: usize) -> u32 {
        (mmio_read32(base + SWPORT_DR_L) & 0xffff)
            | ((mmio_read32(base + SWPORT_DR_H) & 0xffff) << 16)
    }

    /// Writes both GPIO data halves; RK3588 uses bits[31:16] as write enable.
    #[inline]
    fn write_gpio_pair(base: usize, value: u32) {
        mmio_write32(base + SWPORT_DR_L, (value & 0xffff) | 0xffff_0000);
        mmio_write32(base + SWPORT_DR_H, (value >> 16) | 0xffff_0000);
    }

    #[inline]
    fn mmio_read32(vaddr: usize) -> u32 {
        // SAFETY: `vaddr` points inside a window returned by `ax_mm::iomap`
        // that is at least four bytes long, and the register is a 32-bit MMIO
        // register, so the access is aligned and does not alias Rust memory.
        unsafe { (vaddr as *const u32).read_volatile() }
    }

    #[inline]
    fn mmio_write32(vaddr: usize, value: u32) {
        // SAFETY: see `mmio_read32`.
        unsafe { (vaddr as *mut u32).write_volatile(value) }
    }
}

#[cfg(not(target_arch = "aarch64"))]
mod rk3588 {
    /// Non-AArch64 builds keep the benchmark compilable but have no RK3588 pin.
    pub fn init() -> Result<(), &'static str> {
        Ok(())
    }
    pub fn gpio3_output_high() {}
    pub fn gpio3_output_low() {}
    pub fn gpio3_clear_all() {}
    pub fn gpio3_led_red_on() {}
    pub fn gpio3_led_green_on() {}
    pub fn gpio3_ver_id_get() {}
    pub fn gpio3_ext_port_signals_get() {}
}

pub use rk3588::*;
