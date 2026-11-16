use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, ensure};

use super::cache::{self, ToolIdentity};
use crate::support::process::ProcessExt;

#[derive(Default, Debug)]
pub(super) struct CompileStats {
    pub preprocessed: usize,
    pub compiled: usize,
    pub reused: usize,
}
pub(super) struct Compiler {
    tool: ToolIdentity,
    helpers: Vec<Option<ToolIdentity>>,
    pub stats: CompileStats,
}
impl Compiler {
    pub fn new(program: &str) -> anyhow::Result<Self> {
        let tool = cache::tool(program)?;
        let mut helpers = Vec::new();
        for helper in ["cc1", "as"] {
            let output = Command::new(&tool.path)
                .arg(format!("-print-prog-name={helper}"))
                .output()?;
            let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            helpers.push(if output.status.success() && !name.is_empty() {
                cache::optional_tool(&name)?
            } else {
                None
            });
        }
        Ok(Self {
            tool,
            helpers,
            stats: CompileStats::default(),
        })
    }
    pub fn compile_dir(
        &mut self,
        source_dir: &Path,
        obj_dir: &Path,
        flags: &[String],
        source_flags: &BTreeMap<PathBuf, Vec<String>>,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut sources = fs::read_dir(source_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        sources.sort();
        let mut objects = Vec::new();
        for source in sources {
            if source.extension().is_some_and(|ext| ext == "c") {
                let mut flags = flags.to_vec();
                if let Some(extra) = source_flags.get(&source) {
                    flags.extend(extra.clone());
                }
                objects.push(
                    self.compile(&source, obj_dir, &flags)
                        .with_context(|| format!("compile C source {}", source.display()))?,
                );
            }
        }
        Ok(objects)
    }
    fn compile(
        &mut self,
        source: &Path,
        obj_dir: &Path,
        flags: &[String],
    ) -> anyhow::Result<PathBuf> {
        ensure!(source.is_file(), "missing C source {}", source.display());
        fs::create_dir_all(obj_dir)?;
        let temp = tempfile::tempdir_in(obj_dir)?;
        let preprocessed = temp.path().join("input.i");
        let dependencies = temp.path().join("input.d");
        Command::new(&self.tool.path)
            .args(flags)
            .args(["-E", "-MD", "-MT", "deps", "-MF"])
            .arg(&dependencies)
            .arg(source)
            .arg("-o")
            .arg(&preprocessed)
            .exec()?;
        self.stats.preprocessed += 1;
        let key = cache::signature(&(
            &self.tool,
            &self.helpers,
            flags,
            source,
            cache::file_hash(source)?,
            cache::file_hash(&preprocessed)?,
            cache::environment(),
        ))?;
        let object = obj_dir.join(format!("{key}.o"));
        let compiled = cache::artifact(&object, key, |output| {
            Command::new(&self.tool.path)
                .args(flags)
                .arg("-c")
                .arg(&preprocessed)
                .arg("-o")
                .arg(output)
                .exec()
        })?;
        if compiled {
            self.stats.compiled += 1;
        } else {
            self.stats.reused += 1;
        }
        cache::write_unchanged(&object.with_extension("d"), &fs::read(dependencies)?)?;
        Ok(object)
    }
}
pub(super) fn cc_for_arch(arch: &str) -> String {
    format!("{arch}-linux-musl-gcc")
}
pub(super) fn archive_static_lib(
    arch: &str,
    path: &Path,
    objects: &[PathBuf],
) -> anyhow::Result<bool> {
    archive(&format!("{arch}-linux-musl-ar"), path, objects)
}
fn archive(program: &str, path: &Path, objects: &[PathBuf]) -> anyhow::Result<bool> {
    let tool = cache::tool(program)?;
    let key = cache::signature(&(
        &tool,
        "rcs",
        cache::input_hashes(objects)?,
        cache::environment(),
    ))?;
    // The temporary archive starts empty: removed sources cannot leave stale members.
    cache::artifact(path, key, |output| {
        Command::new(&tool.path)
            .arg("rcs")
            .arg(output)
            .args(objects)
            .exec()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preprocessing_tracks_headers_flags_corruption_and_missing_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source with spaces");
        fs::create_dir(&source).unwrap();
        let header = source.join("value.h");
        let input = source.join("value.c");
        let objects = temp.path().join("objects");
        fs::write(&header, "#define VALUE 7\n").unwrap();
        fs::write(
            &input,
            "#include \"value.h\"\n#ifndef EXTRA\n#define EXTRA 0\n#endif\nint value(void) { \
             return VALUE + EXTRA; }\n",
        )
        .unwrap();
        let mut compiler = Compiler::new("cc").unwrap();
        let flags = vec!["-nostdinc".into()];
        let first = compiler.compile(&input, &objects, &flags).unwrap();
        let hash = cache::file_hash(&first).unwrap();
        assert_eq!(compiler.compile(&input, &objects, &flags).unwrap(), first);
        assert_eq!(compiler.stats.compiled, 1);
        assert_eq!(compiler.stats.reused, 1);
        fs::write(&header, "#define VALUE 9\n").unwrap();
        let changed = compiler.compile(&input, &objects, &flags).unwrap();
        assert_ne!(cache::file_hash(&changed).unwrap(), hash);
        let flags = vec!["-nostdinc".into(), "-DEXTRA=1".into()];
        let parameter = compiler.compile(&input, &objects, &flags).unwrap();
        assert_ne!(
            cache::file_hash(&parameter).unwrap(),
            cache::file_hash(&changed).unwrap()
        );
        fs::write(&parameter, "corrupt").unwrap();
        compiler.compile(&input, &objects, &flags).unwrap();
        assert_eq!(compiler.stats.compiled, 4);
        fs::remove_file(header).unwrap();
        assert!(compiler.compile(&input, &objects, &flags).is_err());
    }
    #[test]
    fn archive_reuse_and_source_removal_follow_the_current_object_set() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a.c");
        let b = temp.path().join("b.c");
        fs::write(&a, "int a(void) { return 1; }\n").unwrap();
        fs::write(&b, "int b(void) { return 2; }\n").unwrap();
        let mut compiler = Compiler::new("cc").unwrap();
        let dir = temp.path().join("objects");
        let a = compiler.compile(&a, &dir, &[]).unwrap();
        let b = compiler.compile(&b, &dir, &[]).unwrap();
        let output = temp.path().join("libfixture.a");
        assert!(archive("ar", &output, &[a.clone(), b]).unwrap());
        assert!(archive("ar", &output, &[a.clone(), dir.join("missing")]).is_err());
        assert!(archive("ar", &output, &[a.clone()]).unwrap());
        assert!(!archive("ar", &output, &[a]).unwrap());
        let members = Command::new("ar").arg("t").arg(output).output().unwrap();
        assert!(members.status.success());
        assert_eq!(
            String::from_utf8(members.stdout).unwrap().lines().count(),
            1
        );
    }
}
