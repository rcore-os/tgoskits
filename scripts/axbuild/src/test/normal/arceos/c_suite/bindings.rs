use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use sha2::{Digest, Sha256};

use super::catalog::{Catalog, Selection};

pub(super) struct Bindings {
    pub declarations: String,
    pub selected: String,
}
impl Bindings {
    pub fn new(catalog: &Catalog, selected: &Selection<'_>) -> Self {
        let mut declarations = String::from(
            "#ifndef ARCEOS_C_DECLARATIONS_H\n#define ARCEOS_C_DECLARATIONS_H\n#include \
             <stddef.h>\n",
        );
        for id in &catalog.run_order {
            for test in &catalog.batches[id].tests {
                declarations.push_str(&format!(
                    "int {}(char *reason, size_t reason_len);\n",
                    test.entry
                ));
            }
        }
        declarations.push_str("#endif\n");
        let mut table = String::from(
            "#ifndef ARCEOS_C_SELECTED_H\n#define ARCEOS_C_SELECTED_H\nstatic const struct \
             arceos_c_test_case TESTS[] = {\n",
        );
        for test in &selected.tests {
            table.push_str(&format!(
                "{{{}, {}, {}}},\n",
                literal(&test.id),
                literal(&test.name),
                test.entry
            ));
        }
        table.push_str("};\n");
        table.push_str(&format!(
            "#define ARCEOS_C_BATCH_COMPLETION {}\n#endif\n",
            literal(&selected.completion())
        ));
        Self {
            declarations,
            selected: table,
        }
    }
    pub fn identity(&self) -> String {
        digest(&(self.declarations.clone() + &self.selected))
    }
    pub fn write(&self, root: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
        let declarations = root.join("declarations").join(digest(&self.declarations));
        let selected = root
            .join("selected")
            .join(digest(&self.selected))
            .join("suite_selected.h");
        write_unchanged(
            &declarations.join("suite_declarations.h"),
            self.declarations.as_bytes(),
        )?;
        write_unchanged(&selected, self.selected.as_bytes())?;
        Ok((declarations, selected))
    }
}
pub(super) fn literal(value: &str) -> String {
    serde_json::to_string(value).expect("C string literal")
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn write_unchanged(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let parent = path.parent().context("generated C header directory")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp.write_all(bytes)?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}
