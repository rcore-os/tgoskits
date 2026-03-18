use std::path::{Path, PathBuf};

use super::ctx;
use crate::axvisor::{
    BuildArgs,
    image::{ImageArgs, ImageCommands},
    vmconfig,
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

    let mut vms = vec![];
    match arch.as_str() {
        "aarch64" => {
            vms.push("linux-aarch64-qemu-smp1");
        }
        _ => {} // _ => return Err(anyhow::anyhow!("Unsupported architecture: {}", arch)),
    }

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
        .join(format!("qemu_{arch}_linux"))
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

    let qemu_args = vec![
        "-drive".to_string(),
        format!(
            "id=disk0,if=none,format=raw,file={}",
            image_rootfs.display()
        ),
    ];

    // Read qemu config, insert qemu_args, and save to target/qemu_test_{arch}.toml
    let qemu_config_content = std::fs::read_to_string(&qemu_config)?;
    let mut qemu_config: toml::Table = toml::from_str(&qemu_config_content)?;
    let args = qemu_config
        .entry("args")
        .or_insert_with(|| toml::Value::Array(vec![]));
    if let toml::Value::Array(ref mut arr) = *args {
        for arg in qemu_args {
            arr.push(toml::Value::String(arg));
        }
    }
    let target_dir = tmp_dir.join("qemu-test-targets");
    std::fs::create_dir_all(&target_dir)?;
    let new_qemu_config_path = target_dir.join(format!("qemu_test_{arch}.toml"));
    std::fs::write(&new_qemu_config_path, qemu_config.to_string())?;
    println!("  new qemu config: {}", new_qemu_config_path.display());

    let mut ctx = ctx::Context::new();
    ctx.apply_build_args(&BuildArgs {
        build_dir: None,
        bin_dir: None,
    });

    ctx.vmconfigs = vec![];

    for vm in vms {
        let vm_config_path = PathBuf::from(axvisor_dir)
            .join("configs")
            .join("vms")
            .join(format!("{}.toml", vm));
        ctx.vmconfigs.push(vm_config_path.display().to_string());
    }

    ctx.build_config_path = Some(build_config);
    ctx.run_qemu(Some(new_qemu_config_path)).await?;

    Ok(())
}

fn target_to_arch(target: &str) -> anyhow::Result<String> {
    let sp = target.split('-').collect::<Vec<_>>();
    Ok(sp[0].into())
}
