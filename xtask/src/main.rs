#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    axbuild::run().await?;
    Ok(())
}
