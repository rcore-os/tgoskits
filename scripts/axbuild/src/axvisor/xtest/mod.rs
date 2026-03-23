use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

use super::ctx;
use crate::axvisor::{
    BuildArgs,
    image::{ImageArgs, ImageCommands},
};

fn test_configs() -> Vec<TestConfig> {
    vec![
        TestConfig {
            arch: Arch::Aarch64,
            vms: vec!["linux-aarch64-qemu-smp1"],
            images: vec!["qemu_aarch64_linux"],
        },
        TestConfig {
            arch: Arch::X86_64,
            vms: vec!["nimbos-x86_64-qemu-smp1"],
            images: vec!["qemu_x86_64_nimbos"],
        },
    ]
}

#[derive(Debug, Clone)]
struct TestConfig {
    arch: Arch,
    vms: Vec<&'static str>,
    images: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    #[default]
    Aarch64,
}

impl Arch {
    fn as_str(&self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    fn from_target(target: &str) -> anyhow::Result<Self> {
        let sp = target.split('-').collect::<Vec<_>>();
        match sp[0] {
            "x86_64" => Ok(Arch::X86_64),
            "aarch64" => Ok(Arch::Aarch64),
            _ => Err(anyhow::anyhow!("Unsupported architecture: {}", target)),
        }
    }
}

impl Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

pub async fn run_test_qemu(
    target: Option<impl AsRef<str>>,
    axvisor_dir: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let axvisor_dir = axvisor_dir.as_ref();
    if let Some(target) = target {
        let arch = Arch::from_target(target.as_ref())?;
        run_test_qemu_with_target(arch, axvisor_dir).await?;
    } else {
        let archs = [Arch::X86_64, Arch::Aarch64];
        for arch in archs {
            run_test_qemu_with_target(arch, axvisor_dir).await?;
        }
    }

    Ok(())
}

async fn run_test_qemu_with_target_vms(
    arch: Arch,
    vms: Vec<String>,
    images: Vec<String>,
    axvisor_dir: impl AsRef<Path>,
) -> anyhow::Result<()> {
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

    let tmp_dir = std::env::temp_dir();
    let image_dir = tmp_dir.join(".axvisor-images");

    println!("  required images:");
    for image_name in &images {
        println!("    - {image_name}");
    }

    let image_rootfs = ensure_images(&images, &image_dir, axvisor_dir).await?;
    println!("  image rootfs: {}", image_rootfs.display());

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

    let mut ctx = ctx::Context::new(axvisor_dir);
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

pub async fn run_test_qemu_with_target(
    arch: Arch,
    axvisor_dir: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let tests = arch_tests(arch);
    for test in tests {
        run_test_qemu_with_target_vms(
            arch,
            test.vms.into_iter().map(String::from).collect(),
            test.images.into_iter().map(String::from).collect(),
            &axvisor_dir,
        )
        .await?;
    }

    Ok(())
}

fn arch_tests(arch: Arch) -> Vec<TestConfig> {
    test_configs()
        .into_iter()
        .filter(|config| config.arch == arch)
        .collect()
}

async fn ensure_images(
    images: &[String],
    image_dir: &Path,
    repo_root: &Path,
) -> anyhow::Result<PathBuf> {
    let mut rootfs_path = None;

    for image_name in images {
        let extract_dir = image_dir.join(image_name);
        let candidate_rootfs = extract_dir.join("rootfs.img");

        if !candidate_rootfs.exists() {
            let image = ImageArgs {
                overrides: Default::default(),
                command: ImageCommands::Download {
                    image_name: image_name.clone(),
                    output_dir: Some(image_dir.to_string_lossy().into_owned()),
                    no_extract: false,
                },
            };

            image.execute(repo_root).await?;
        } else {
            println!("  image already extracted: {}", extract_dir.display());
        }

        if rootfs_path.is_none() && candidate_rootfs.exists() {
            rootfs_path = Some(candidate_rootfs);
        }
    }

    rootfs_path.ok_or_else(|| anyhow::anyhow!("No rootfs.img found in required images"))
}
