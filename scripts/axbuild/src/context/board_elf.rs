use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, ensure};
use tempfile::TempDir;

use crate::support::process::ProcessExt;

/// Owns the transfer copy until the board runner has stopped using it.
pub(super) struct BoardElf {
    path: PathBuf,
    _staging: TempDir,
}

impl BoardElf {
    pub(super) fn prepare(source: &Path, artifact_dir: &Path) -> anyhow::Result<Self> {
        let directory = artifact_dir.join("board-elf");
        fs::create_dir_all(&directory)?;
        let staging = tempfile::tempdir_in(&directory)?;
        let path = staging
            .path()
            .join(source.file_name().context("board ELF has no file name")?);
        // Keep the original DWARF for host debugging and the symbol table for
        // EFI entry lookup. Only non-loadable debug sections are omitted.
        Command::new(llvm_objcopy()?)
            .arg("--strip-debug")
            .arg(source)
            .arg(&path)
            .exec()
            .with_context(|| format!("failed to prepare board ELF {}", source.display()))?;
        Ok(Self {
            path,
            _staging: staging,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

fn llvm_objcopy() -> anyhow::Result<PathBuf> {
    let output = Command::new("rustc")
        .args(["--print", "target-libdir"])
        .output()
        .context("failed to locate the Rust toolchain LLVM tools")?;
    ensure!(
        output.status.success(),
        "rustc --print target-libdir failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let library = String::from_utf8(output.stdout).context("invalid Rust toolchain path")?;
    let tool = Path::new(library.trim())
        .parent()
        .context("Rust target library directory has no parent")?
        .join("bin")
        .join(format!("llvm-objcopy{}", std::env::consts::EXE_SUFFIX));
    ensure!(
        tool.is_file(),
        "Rust llvm-tools component is missing: {}",
        tool.display()
    );
    Ok(tool)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use object::{Object, ObjectSection, ObjectSymbol};

    use super::*;

    #[test]
    fn transfer_preserves_boot_symbols_and_code_without_changing_debug_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("payload.rs");
        let elf = root.path().join("payload.o");
        fs::write(
            &source,
            "#![no_std]\n#[unsafe(no_mangle)] pub extern \"C\" fn payload_entry() -> usize { 7 }\n",
        )
        .unwrap();
        Command::new("rustc")
            .args([
                "--crate-type=lib",
                "--edition=2024",
                "--emit=obj",
                "-g",
                "-o",
            ])
            .arg(&elf)
            .arg(&source)
            .exec()
            .unwrap();
        let original = fs::read(&elf).unwrap();
        let original_object = object::File::parse(original.as_slice()).unwrap();
        assert!(original_object.section_by_name(".debug_info").is_some());

        let transfer = BoardElf::prepare(&elf, root.path()).unwrap();
        let path = transfer.path().to_path_buf();
        let bytes = fs::read(&path).unwrap();
        let object = object::File::parse(bytes.as_slice()).unwrap();
        assert!(object.section_by_name(".debug_info").is_none());
        let entry = |file: &object::File<'_>| {
            file.symbols()
                .find(|symbol| symbol.name() == Ok("payload_entry"))
                .expect("boot entry symbol must survive transfer preparation")
                .address()
        };
        assert_eq!(entry(&object), entry(&original_object));
        for section in original_object.sections() {
            if section.kind() == object::SectionKind::Text {
                let transferred = object
                    .section_by_name(section.name().unwrap())
                    .expect("executable section must survive transfer preparation");
                assert_eq!(transferred.address(), section.address());
                assert_eq!(transferred.data().unwrap(), section.data().unwrap());
            }
        }
        assert_eq!(fs::read(&elf).unwrap(), original);
        drop(transfer);
        assert!(!path.exists());
        assert!(elf.is_file());
    }
}
