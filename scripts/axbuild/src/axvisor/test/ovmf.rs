//! OVMF image preparation for nested x86 Axvisor tests.

use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use ostool::{build::config::Cargo, ovmf::Arch};
use tempfile::NamedTempFile;

use crate::{
    context::ResolvedAxvisorRequest,
    support::{download::file_sha256, ovmf::OvmfFirmware},
};

const OVMF_SIZE: usize = 4 * 1024 * 1024;
const OUTPUT_ENV: &str = "AXVISOR_TEST_X86_OVMF_OUTPUT";
const INPUT_ENV: &str = "AXVISOR_TEST_X86_OVMF_INPUT";
const VARS_INPUT_ENV: &str = "AXVISOR_TEST_X86_OVMF_VARS_INPUT";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OvmfLayout {
    SplitCodeVars,
    MonolithicCode,
}

impl fmt::Display for OvmfLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SplitCodeVars => formatter.write_str("split CODE/VARS"),
            Self::MonolithicCode => formatter.write_str("monolithic CODE"),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct FirmwareFileEvidence {
    path: PathBuf,
    size: u64,
    sha256: String,
}

impl FirmwareFileEvidence {
    fn collect(path: &Path) -> anyhow::Result<Self> {
        let size = fs::metadata(path)
            .with_context(|| format!("failed to inspect firmware {}", path.display()))?
            .len();
        Ok(Self {
            path: path.to_path_buf(),
            size,
            sha256: file_sha256(path)?,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct OvmfEvidence {
    layout: OvmfLayout,
    code: FirmwareFileEvidence,
    vars: Option<FirmwareFileEvidence>,
    guest: FirmwareFileEvidence,
}

impl fmt::Display for OvmfEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Axvisor x86 OVMF firmware evidence:\nlayout: {}\nOVMF CODE: path={} size={} sha256={}",
            self.layout,
            self.code.path.display(),
            self.code.size,
            self.code.sha256,
        )?;
        if let Some(vars) = &self.vars {
            write!(
                formatter,
                "\nOVMF VARS: path={} size={} sha256={} usage=prefix",
                vars.path.display(),
                vars.size,
                vars.sha256,
            )?;
        } else {
            formatter.write_str("\nOVMF VARS: unused")?;
        }
        write!(
            formatter,
            "\nguest image: path={} size={} sha256={}",
            self.guest.path.display(),
            self.guest.size,
            self.guest.sha256,
        )
    }
}

pub(super) async fn prepare_configured_x86_ovmf(
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    let output = cargo.env.get(OUTPUT_ENV);
    let input = cargo.env.get(INPUT_ENV);
    let vars_input = cargo.env.get(VARS_INPUT_ENV);
    ensure!(
        output.is_some() || input.is_none(),
        "{INPUT_ENV} requires {OUTPUT_ENV}"
    );
    ensure!(
        input.is_some() || vars_input.is_none(),
        "{VARS_INPUT_ENV} requires {INPUT_ENV}"
    );
    let Some(output) = output else {
        return Ok(());
    };
    ensure!(
        request.arch == "x86_64",
        "{OUTPUT_ENV} is only valid for x86_64 Axvisor tests"
    );

    let output_path = super::assets::resolve_workspace_path(workspace_root, output, OUTPUT_ENV)?;
    let evidence = if let Some(input) = input {
        let input_path = super::assets::resolve_workspace_path(workspace_root, input, INPUT_ENV)?;
        if let Some(vars_input) = vars_input {
            let vars_path =
                super::assets::resolve_workspace_path(workspace_root, vars_input, VARS_INPUT_ENV)?;
            prepare_x86_ovmf_from_split_paths(&output_path, &input_path, &vars_path)?
        } else {
            prepare_x86_ovmf_from_monolithic_path(&output_path, &input_path)?
        }
    } else {
        prepare_x86_ovmf(&output_path).await?
    };
    println!("{evidence}");
    Ok(())
}

pub(super) async fn prepare_x86_ovmf(output_path: &Path) -> anyhow::Result<OvmfEvidence> {
    let firmware = OvmfFirmware::fetch(Arch::X64).await?;
    prepare_x86_ovmf_from_firmware(output_path, &firmware)
}

fn prepare_x86_ovmf_from_monolithic_path(
    output_path: &Path,
    code_path: &Path,
) -> anyhow::Result<OvmfEvidence> {
    prepare_x86_ovmf_from_paths(output_path, code_path, None)
}

fn prepare_x86_ovmf_from_split_paths(
    output_path: &Path,
    code_path: &Path,
    vars_path: &Path,
) -> anyhow::Result<OvmfEvidence> {
    prepare_x86_ovmf_from_paths(output_path, code_path, Some(vars_path))
}

fn prepare_x86_ovmf_from_firmware(
    output_path: &Path,
    firmware: &OvmfFirmware,
) -> anyhow::Result<OvmfEvidence> {
    let code_path = firmware.code();
    let code_size = fs::metadata(code_path)
        .with_context(|| format!("failed to inspect OVMF code image {}", code_path.display()))?
        .len();
    let vars_path = (code_size != OVMF_SIZE as u64).then(|| firmware.vars());
    prepare_x86_ovmf_from_paths(output_path, code_path, vars_path)
}

fn prepare_x86_ovmf_from_paths(
    output_path: &Path,
    code_path: &Path,
    vars_path: Option<&Path>,
) -> anyhow::Result<OvmfEvidence> {
    let code = fs::read(code_path)
        .with_context(|| format!("failed to read OVMF code image {}", code_path.display()))?;
    let (layout, vars) = if code.len() == OVMF_SIZE {
        ensure!(
            vars_path.is_none(),
            "a 4 MiB monolithic OVMF image must not specify a VARS prefix"
        );
        (OvmfLayout::MonolithicCode, None)
    } else {
        OVMF_SIZE.checked_sub(code.len()).with_context(|| {
            format!(
                "OVMF code image {} is larger than 4 MiB",
                code_path.display()
            )
        })?;
        let vars_path = vars_path.context("split OVMF CODE requires a VARS prefix")?;
        (
            OvmfLayout::SplitCodeVars,
            Some(fs::read(vars_path).with_context(|| {
                format!("failed to read OVMF variable store {}", vars_path.display())
            })?),
        )
    };
    let image = assemble_ovmf_image(&code, vars.as_deref())?;
    install_ovmf_image(output_path, &image)?;

    let vars = vars_path.map(FirmwareFileEvidence::collect).transpose()?;
    Ok(OvmfEvidence {
        layout,
        code: FirmwareFileEvidence::collect(code_path)?,
        vars,
        guest: FirmwareFileEvidence::collect(output_path)?,
    })
}

fn install_ovmf_image(output_path: &Path, image: &[u8]) -> anyhow::Result<()> {
    let parent = output_path
        .parent()
        .with_context(|| format!("OVMF output path has no parent: {}", output_path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create OVMF output directory {}",
            parent.display()
        )
    })?;
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary OVMF image in {}",
            parent.display()
        )
    })?;
    temporary
        .write_all(image)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    temporary
        .persist(output_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install {}", output_path.display()))?;
    Ok(())
}

fn assemble_ovmf_image(code: &[u8], vars: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
    ensure!(
        code.len() <= OVMF_SIZE,
        "OVMF code image is larger than 4 MiB"
    );
    let prefix_len = OVMF_SIZE - code.len();
    let vars = vars.unwrap_or_default();
    ensure!(
        vars.len() == prefix_len,
        "OVMF variable-store prefix is {:#x} bytes, expected {prefix_len:#x}",
        vars.len()
    );

    let mut image = Vec::with_capacity(OVMF_SIZE);
    image.extend_from_slice(vars);
    image.extend_from_slice(code);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn split_ovmf_is_assembled_at_the_top_of_the_four_mib_window() {
        let vars = vec![0xa5; 0x84_000];
        let code = vec![0x5a; OVMF_SIZE - vars.len()];
        let image = assemble_ovmf_image(&code, Some(&vars)).unwrap();

        assert_eq!(image.len(), OVMF_SIZE);
        assert_eq!(&image[..vars.len()], vars);
        assert_eq!(&image[vars.len()..], code);
        assert!(assemble_ovmf_image(&code, None).is_err());
    }

    #[test]
    fn split_firmware_evidence_matches_the_installed_guest_image() {
        let root = tempdir().unwrap();
        let code_path = root.path().join("OVMF_CODE.fd");
        let vars_path = root.path().join("OVMF_VARS.fd");
        let output_path = root.path().join("guest/OVMF_CODE_4M.fd");
        let vars = vec![0xa5; 0x84_000];
        let code = vec![0x5a; OVMF_SIZE - vars.len()];
        fs::write(&code_path, &code).unwrap();
        fs::write(&vars_path, &vars).unwrap();
        let firmware = OvmfFirmware::from_paths(code_path.clone(), vars_path.clone());

        let evidence = prepare_x86_ovmf_from_firmware(&output_path, &firmware).unwrap();
        let image = fs::read(&output_path).unwrap();

        assert_eq!(evidence.layout, OvmfLayout::SplitCodeVars);
        assert_eq!(image.len(), OVMF_SIZE);
        assert_eq!(&image[..vars.len()], vars);
        assert_eq!(&image[vars.len()..], code);
        assert_eq!(evidence.code.path, code_path);
        assert_eq!(evidence.code.size, code.len() as u64);
        assert_eq!(evidence.code.sha256, file_sha256(&code_path).unwrap());
        let evidence_vars = evidence.vars.as_ref().unwrap();
        assert_eq!(evidence_vars.path, vars_path);
        assert_eq!(evidence_vars.size, vars.len() as u64);
        assert_eq!(evidence_vars.sha256, file_sha256(&vars_path).unwrap());
        assert_eq!(evidence.guest.path, output_path);
        assert_eq!(evidence.guest.size, OVMF_SIZE as u64);
        assert_eq!(evidence.guest.sha256, file_sha256(&output_path).unwrap());

        let text = evidence.to_string();
        assert!(text.contains("layout: split CODE/VARS"));
        assert!(text.contains(&format!("OVMF CODE: path={}", code_path.display())));
        assert!(text.contains(&format!("OVMF VARS: path={}", vars_path.display())));
        assert!(text.contains("usage=prefix"));
        assert!(text.contains(&format!("guest image: path={}", output_path.display())));
    }

    #[test]
    fn monolithic_firmware_reports_the_variable_store_as_unused() {
        let root = tempdir().unwrap();
        let code_path = root.path().join("OVMF_CODE.fd");
        let output_path = root.path().join("OVMF_CODE_4M.fd");
        fs::write(&code_path, vec![0x5a; OVMF_SIZE]).unwrap();

        let evidence = prepare_x86_ovmf_from_monolithic_path(&output_path, &code_path).unwrap();

        assert_eq!(evidence.layout, OvmfLayout::MonolithicCode);
        assert!(evidence.vars.is_none());
        assert!(evidence.to_string().contains("OVMF VARS: unused"));
        assert_eq!(fs::metadata(output_path).unwrap().len(), OVMF_SIZE as u64);
    }
}
