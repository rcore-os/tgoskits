//! Content fingerprints and atomic publication for the C build outputs.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// Bump when cache interpretation changes, independently of crate publishing.
const CACHE_VERSION: u32 = 2;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ToolIdentity {
    pub path: PathBuf,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Artifact {
    version: u32,
    key: String,
    sha256: String,
}
pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn file_hash(path: &Path) -> anyhow::Result<String> {
    Ok(digest(&fs::read(path).with_context(|| {
        format!("read build input {}", path.display())
    })?))
}
pub(super) fn signature(value: &impl Serialize) -> anyhow::Result<String> {
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut rows: Vec<_> = map.into_iter().collect();
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::Value::Object(
                    rows.into_iter()
                        .map(|(key, value)| (key, canonical(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(rows) => {
                serde_json::Value::Array(rows.into_iter().map(canonical).collect())
            }
            value => value,
        }
    }
    Ok(digest(&serde_json::to_vec(&canonical(
        serde_json::to_value((CACHE_VERSION, value))?,
    ))?))
}
// File metadata is not a content identity: in-place writes can share ctime
// granularity, and callers can restore mtime. Always verify tool bytes.
pub(super) fn optional_tool(program: &str) -> anyhow::Result<Option<ToolIdentity>> {
    let Some(path) = crate::support::process::find_optional_host_binary(program) else {
        return Ok(None);
    };
    Ok(Some(ToolIdentity {
        sha256: file_hash(&path)?,
        path,
    }))
}
pub(super) fn tool(program: &str) -> anyhow::Result<ToolIdentity> {
    optional_tool(program)?.with_context(|| format!("missing C build tool {program}"))
}
pub(super) fn input_hashes(paths: &[PathBuf]) -> anyhow::Result<Vec<(PathBuf, String)>> {
    paths
        .iter()
        .map(|path| Ok((path.clone(), file_hash(path)?)))
        .collect()
}
pub(super) fn environment() -> BTreeMap<String, String> {
    [
        "PATH",
        "SOURCE_DATE_EPOCH",
        "GCC_EXEC_PREFIX",
        "COMPILER_PATH",
        "CPATH",
        "C_INCLUDE_PATH",
        "LIBRARY_PATH",
        "LD_LIBRARY_PATH",
        "LC_ALL",
        "LANG",
    ]
    .into_iter()
    .filter_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| (name.to_owned(), value))
    })
    .collect()
}
pub(super) fn write_unchanged(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let directory = path.parent().context("build output directory")?;
    fs::create_dir_all(directory)?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.write_all(bytes)?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}
pub(super) fn artifact(
    output: &Path,
    key: String,
    build: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let metadata = output.with_file_name(format!(
        "{}.cache.json",
        output
            .file_name()
            .context("artifact filename")?
            .to_string_lossy()
    ));
    if let Ok(bytes) = fs::read(&metadata)
        && let Ok(saved) = serde_json::from_slice::<Artifact>(&bytes)
        && saved.version == CACHE_VERSION
        && saved.key == key
        && file_hash(output).is_ok_and(|hash| hash == saved.sha256)
    {
        return Ok(false);
    }
    let directory = output.parent().context("artifact directory")?;
    fs::create_dir_all(directory)?;
    let temp = tempfile::tempdir_in(directory)?;
    let temporary = temp
        .path()
        .join(output.file_name().context("artifact filename")?);
    build(&temporary)?;
    ensure!(
        temporary.is_file(),
        "builder did not produce {}",
        temporary.display()
    );
    let saved = Artifact {
        version: CACHE_VERSION,
        key,
        sha256: file_hash(&temporary)?,
    };
    fs::rename(&temporary, output)?;
    write_unchanged(&metadata, &serde_json::to_vec(&saved)?)?;
    Ok(true)
}

#[derive(Serialize, Deserialize)]
struct SymbolMaps {
    version: u32,
    key: String,
    axbt: String,
    axks: String,
}
pub(crate) fn ensure_symbol_maps(elf: &Path) -> anyhow::Result<bool> {
    let axbt = elf.with_extension("axbt");
    let axks = elf.with_extension("axks");
    let metadata = elf.with_extension("symbol-cache.json");
    // These are fingerprints of the compiled generator, not runtime source inspection.
    let elf_hash = file_hash(elf)?;
    let key = signature(&(
        elf_hash.clone(),
        digest(include_bytes!("../../build/symbol_map.rs")),
        digest(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../Cargo.lock"
        ))),
        digest(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rust-toolchain.toml"
        ))),
    ))?;
    if let Ok(bytes) = fs::read(&metadata)
        && let Ok(saved) = serde_json::from_slice::<SymbolMaps>(&bytes)
        && saved.version == CACHE_VERSION
        && saved.key == key
        && file_hash(&axbt).is_ok_and(|hash| hash == saved.axbt)
        && file_hash(&axks).is_ok_and(|hash| hash == saved.axks)
    {
        println!("[axbuild] C symbol maps reused");
        return Ok(false);
    }
    let temp = tempfile::tempdir_in(elf.parent().context("ELF directory")?)?;
    let temporary = temp.path().join("symbols.axbt");
    crate::build::symbol_map::generate_axbt_map(elf, &temporary)?;
    ensure!(
        file_hash(elf)? == elf_hash,
        "ELF changed while generating C symbol maps"
    );
    let other = temporary.with_extension("axks");
    let saved = SymbolMaps {
        version: CACHE_VERSION,
        key,
        axbt: file_hash(&temporary)?,
        axks: file_hash(&other)?,
    };
    fs::rename(temporary, axbt)?;
    fs::rename(other, axks)?;
    write_unchanged(&metadata, &serde_json::to_vec(&saved)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifact_identity_is_order_independent_and_failed_builds_are_not_published() {
        let first = std::collections::HashMap::from([("a", 1), ("b", 2)]);
        let second = std::collections::HashMap::from([("b", 2), ("a", 1)]);
        assert_eq!(signature(&first).unwrap(), signature(&second).unwrap());
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("result");
        assert!(
            artifact(&output, "old".into(), |path| {
                fs::write(path, "old")?;
                Ok(())
            })
            .unwrap()
        );
        let error = artifact(&output, "new".into(), |path| {
            fs::write(path, "partial")?;
            anyhow::bail!("build failed")
        });
        assert!(error.is_err());
        assert_eq!(fs::read_to_string(&output).unwrap(), "old");
        assert!(
            !artifact(&output, "old".into(), |_| panic!(
                "valid output should be reused"
            ))
            .unwrap()
        );
        fs::write(&output, "corrupted").unwrap();
        assert!(
            artifact(&output, "old".into(), |path| {
                fs::write(path, "old")?;
                Ok(())
            })
            .unwrap()
        );
    }
    #[test]
    fn symbol_cache_requires_both_outputs_and_rejects_corruption() {
        use std::process::Command;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("main.c");
        let elf = temp.path().join("program");
        fs::write(&source, "int main(void) { return 0; }\n").unwrap();
        assert!(
            Command::new("cc")
                .arg("-g")
                .arg(&source)
                .arg("-o")
                .arg(&elf)
                .status()
                .unwrap()
                .success()
        );
        assert!(ensure_symbol_maps(&elf).unwrap());
        assert!(!ensure_symbol_maps(&elf).unwrap());
        fs::remove_file(elf.with_extension("axks")).unwrap();
        assert!(ensure_symbol_maps(&elf).unwrap());
        fs::write(elf.with_extension("axbt"), "broken").unwrap();
        assert!(ensure_symbol_maps(&elf).unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn tool_replacement_invalidates_identity_even_with_preserved_size_and_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("compiler");
        fs::write(&program, "first").unwrap();
        let modified = fs::metadata(&program).unwrap().modified().unwrap();
        let first = tool(program.to_str().unwrap()).unwrap();
        assert_eq!(
            signature(&first).unwrap(),
            signature(&tool(program.to_str().unwrap()).unwrap()).unwrap()
        );
        fs::write(&program, "other").unwrap();
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&program)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let second = tool(program.to_str().unwrap()).unwrap();
        assert_ne!(signature(&first).unwrap(), signature(&second).unwrap());
    }
}
