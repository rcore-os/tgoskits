use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context, ensure};
use clap::Args;
use ostool::ovmf::*;
use serde::Serialize;

const CACHE_DIR_ENV: &str = "TGOS_OVMF_DIR";

#[derive(Args, Clone, Debug)]
pub(crate) struct OvmfArgs {
    /// OVMF architecture: x86_64, aarch64, riscv64, loongarch64, or ia32
    #[arg(long, value_parser = parse_arch)]
    pub(crate) arch: Arch,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct OvmfFirmware {
    code: PathBuf,
    vars: PathBuf,
}

impl OvmfFirmware {
    pub(crate) async fn fetch(arch: Arch) -> anyhow::Result<Self> {
        let cache_dir = selected_cache_dir(std::env::var_os(CACHE_DIR_ENV))?;
        let prebuilt = Prebuilt::fetch(Source::LATEST, &cache_dir)
            .await
            .with_context(|| format!("failed to prepare OVMF cache {}", cache_dir.display()))?;
        let firmware = Self {
            code: prebuilt.get_file(arch, FileType::Code),
            vars: prebuilt.get_file(arch, FileType::Vars),
        };
        ensure!(
            firmware.code.is_file() && firmware.vars.is_file(),
            "OVMF cache {} does not contain code and vars firmware for {}",
            cache_dir.display(),
            arch.as_str()
        );
        Ok(firmware)
    }

    pub(crate) fn code(&self) -> &std::path::Path {
        &self.code
    }

    pub(crate) fn vars(&self) -> &std::path::Path {
        &self.vars
    }

    #[cfg(test)]
    pub(crate) fn from_paths(code: PathBuf, vars: PathBuf) -> Self {
        Self { code, vars }
    }
}

pub(crate) async fn execute(args: OvmfArgs) -> anyhow::Result<()> {
    let firmware = OvmfFirmware::fetch(args.arch).await?;
    println!("{}", serde_json::to_string(&firmware)?);
    Ok(())
}

fn selected_cache_dir(override_dir: Option<OsString>) -> anyhow::Result<PathBuf> {
    let Some(override_dir) = override_dir else {
        return Ok(default_cache_dir());
    };
    ensure!(
        !override_dir.is_empty(),
        "{CACHE_DIR_ENV} must not be empty"
    );
    Ok(PathBuf::from(override_dir))
}

fn parse_arch(value: &str) -> Result<Arch, String> {
    match value {
        "x86_64" | "x64" => Ok(Arch::X64),
        "aarch64" => Ok(Arch::Aarch64),
        "riscv64" => Ok(Arch::Riscv64),
        "loongarch64" => Ok(Arch::LoongArch64),
        "i386" | "ia32" => Ok(Arch::Ia32),
        _ => Err(format!("unsupported OVMF architecture '{value}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_and_json_contract_are_stable() {
        let override_dir = PathBuf::from("/cache/ovmf");
        assert_eq!(
            selected_cache_dir(Some(override_dir.clone().into_os_string())).unwrap(),
            override_dir
        );
        assert_eq!(selected_cache_dir(None).unwrap(), default_cache_dir());
        assert!(selected_cache_dir(Some(OsString::new())).is_err());

        let json = serde_json::to_value(OvmfFirmware {
            code: PathBuf::from("/cache/ovmf/x64/code.fd"),
            vars: PathBuf::from("/cache/ovmf/x64/vars.fd"),
        })
        .unwrap();
        assert_eq!(json["code"], "/cache/ovmf/x64/code.fd");
        assert_eq!(json["vars"], "/cache/ovmf/x64/vars.fd");
    }
}
