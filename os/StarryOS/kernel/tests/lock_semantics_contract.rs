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

fn compact_source(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
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

#[test]
fn tty_blocking_ownership_uses_pi_mutexes() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let tty = compact_source(&source_root.join("pseudofs/dev/tty/mod.rs"));
    let line_discipline = compact_source(&source_root.join("pseudofs/dev/tty/terminal/ldisc.rs"));

    assert!(
        tty.contains("ldisc:PiMutex<LineDiscipline<R,W>>"),
        "the tty line-discipline owner crosses backend waits and must use PiMutex"
    );
    assert!(
        line_discipline.contains("InterruptDriven(Arc<PiMutex<InputReader<R,W>>>)"),
        "the interrupt-driven reader owner crosses backend waits and must use PiMutex"
    );
}

#[test]
fn single_cpu_pi_mutex_does_not_spin_on_its_owner() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("Starry kernel must remain below the workspace root");
    let mutex = compact_source(&workspace_root.join("components/ax-task/src/sync/mutex/mod.rs"));
    let entry = compact_source(&workspace_root.join("components/ax-task/src/sync/mutex/entry.rs"));

    assert!(
        mutex.contains("owner_spin_eligible(cpu_count")
            && entry.contains("cpu_count>1")
            && entry.contains("same_owner&&owner_on_cpu&&waiter_is_top&&!need_resched"),
        "Linux PREEMPT_RT disables rtmutex owner spinning on a single-CPU kernel"
    );
}

#[test]
fn exec_cloexec_table_guard_does_not_cross_the_commit_boundary() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let execve = std::fs::read_to_string(source_root.join("syscall/task/execve.rs"))
        .expect("execve source must be readable");
    let commit_boundary = execve
        .find("Phase 2: point of no return")
        .expect("execve must name its commit boundary");
    let preparation = &execve[..commit_boundary];

    assert!(
        !preparation.contains("let fd_table_owner = current_fd_table();"),
        "exec preparation must not retain the FD table write guard across address-space commit"
    );
}
