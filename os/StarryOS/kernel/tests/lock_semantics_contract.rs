//! Source contract requiring every Starry mutex to name its blocking semantics.

use std::path::{Path, PathBuf};

fn rust_sources(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).expect("Starry source directory must be readable") {
        let path = entry.expect("Starry source entry must be readable").path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn imports_ambiguous_mutex(compact_source: &str) -> bool {
    let mut remaining = compact_source;
    while let Some((_, after_use)) = remaining.split_once("useax_sync::{") {
        let Some((imports, rest)) = after_use.split_once("};") else {
            return true;
        };
        if imports
            .split(',')
            .any(|import| import == "Mutex" || import.starts_with("Mutexas"))
        {
            return true;
        }
        remaining = rest;
    }
    false
}

#[test]
fn starry_mutexes_name_pi_or_spin_semantics() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&source_root, &mut files);
    files.sort();

    let mut violations = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("Starry source file must be readable");
        let compact = source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        if compact.contains("ax_sync::Mutex") || imports_ambiguous_mutex(&compact) {
            violations.push(
                path.strip_prefix(&source_root)
                    .expect("source path must remain below source root")
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        violations.is_empty(),
        "Starry locks must use explicit PiMutex or SpinMutex semantics; ambiguous ax_sync::Mutex \
         in: {}",
        violations.join(", ")
    );
}
