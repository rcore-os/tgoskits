#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]

#[cfg(any(windows, unix))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    axbuild::run().await?;
    Ok(())
}

#[cfg(not(any(windows, unix)))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(not(any(windows, unix)))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
