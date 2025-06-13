use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

const TEMPLATE_DIR: &str = "template";

fn get_files_recursively(dir: &Path, prefix: String) -> io::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let path_name = path.file_name().unwrap().to_str().unwrap();
        let full_name = if prefix.is_empty() {
            path_name.into()
        } else {
            format!("{prefix}/{path_name}")
        };
        if path.is_dir() {
            files.extend(get_files_recursively(&path, full_name)?);
        } else {
            files.push(full_name);
        }
    }
    Ok(files)
}

fn main() -> io::Result<()> {
    let src_dir = Path::new(std::env!("CARGO_MANIFEST_DIR")).join(TEMPLATE_DIR);
    let out_path = Path::new(std::env!("CARGO_MANIFEST_DIR")).join("src/template.rs");

    let mut f = File::create(out_path)?;
    writeln!(f, "pub const TEMPLATE: &[(&str, &str)] = &[")?;

    let files = get_files_recursively(&src_dir, "".into())?;
    for file in files {
        writeln!(
            f,
            "    (\"{file}\", include_str!(\"../{TEMPLATE_DIR}/{file}\")),",
        )?;
    }
    writeln!(f, "];")?;
    Ok(())
}
