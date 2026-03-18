use std::path::{Path, PathBuf};

use super::ctx;
use crate::axvisor::{
    BuildArgs,
    image::{ImageArgs, ImageCommands},
};

pub async fn run_test_qemu(
    target: Option<impl AsRef<str>>,
    axvisor_dir: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let axvisor_dir = axvisor_dir.as_ref();
    if let Some(target) = target {
        run_test_qemu_with_target(target, axvisor_dir).await?;
    } else {
        let targets = ["x86_64", "aarch64"];
        for target in &targets {
            run_test_qemu_with_target(target, axvisor_dir).await?;
        }
    }

    Ok(())
}

pub async fn run_test_qemu_with_target(
    target: impl AsRef<str>,
    axvisor_dir: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let arch = target_to_arch(target.as_ref())?;
    let axvisor_dir = axvisor_dir.as_ref();

    let qemu_config = PathBuf::from(axvisor_dir)
        .join(".github")
        .join("workflows")
        .join(format!("qemu-{arch}.toml"));
    let build_config = PathBuf::from(axvisor_dir)
        .join("configs")
        .join("board")
        .join(format!("qemu-{arch}.toml"));

    println!("Running qemu test for {arch}");
    println!("  build config: {}", build_config.display());
    println!("  qemu config: {}", qemu_config.display());
    let image_name = format!("qemu_{arch}_linux");

    let tmp_dir = std::env::temp_dir();

    let image_dir = tmp_dir.join(".axvisor-images");
    let image_rootfs = image_dir
        .join(format!("qemu-{arch}-linux"))
        .join("rootfs.img");
    println!("  image rootfs: {}", image_rootfs.display());

    let image = ImageArgs {
        overrides: Default::default(),
        command: ImageCommands::Download {
            image_name,
            output_dir: Some(image_dir.to_str().unwrap().into()),
            no_extract: false,
        },
    };

    image.execute().await?;

    let qemu_args = [
        "-drive".to_string(),
        format!(
            "id=disk0,if=none,format=raw,file={}",
            image_rootfs.display()
        ),
    ];

    let mut ctx = ctx::Context::new();
    ctx.apply_build_args(&BuildArgs {
        build_dir: None,
        bin_dir: None,
    });
    ctx.vmconfigs = vec![];
    ctx.build_config_path = Some(build_config);
    ctx.run_qemu(Some(qemu_config)).await?;

    Ok(())
}

fn target_to_arch(target: &str) -> anyhow::Result<String> {
    let sp = target.split('-').collect::<Vec<_>>();
    Ok(sp[0].into())
}
