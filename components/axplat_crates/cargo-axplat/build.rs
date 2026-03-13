use std::env;
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
    let src_dir = Path::new(&env::var_os("CARGO_MANIFEST_DIR").unwrap()).join(TEMPLATE_DIR);
    let out_path = Path::new(&env::var_os("OUT_DIR").unwrap()).join("template.rs");

    let mut f = File::create(out_path)?;
    writeln!(f, "pub const TEMPLATE: &[(&str, &str)] = &[")?;

    let files = get_files_recursively(&src_dir, "".into())?;
    for file in files {
        let src_dir = src_dir.display();
        if file == "_Cargo.toml" {
            // `Cargo.toml` is not allowed to be included as a template file in
            // a cargo package , use `_Cargo.toml` instead
            writeln!(
                f,
                "    (\"Cargo.toml\", include_str!(\"{src_dir}/_Cargo.toml\")),",
            )?;
        } else {
            writeln!(f, "    (\"{file}\", include_str!(\"{src_dir}/{file}\")),",)?;
        }
    }
    writeln!(f, "];")?;
    Ok(())
}
