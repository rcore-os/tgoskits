//! Source-level contracts for transactional init-process resource publication.

const ENTRY: &str = include_str!("../src/entry.rs");
const PROCESS_SCOPE: &str = include_str!("../src/file/process_scope.rs");
const TASK: &str = include_str!("../src/task/mod.rs");

#[test]
fn boot_prepares_a_complete_scope_before_process_data_exists() {
    let handover = ENTRY
        .find("prepare_console_handover")
        .expect("boot must reserve console ownership before opening stdio");
    let scope_prepare = ENTRY
        .find("PreparedProcessScope::prepare_init")
        .expect("boot must prepare one complete process scope");
    let process_create = ENTRY
        .find("ProcessData::new")
        .expect("boot must create ProcessData after scope preparation succeeds");

    assert!(
        handover < scope_prepare && scope_prepare < process_create,
        "fallible filesystem and stdio preparation must finish before ProcessData is constructible"
    );
    assert!(
        ENTRY.contains("drop(console_handover)"),
        "scope preparation failure must explicitly roll back the console reservation before panic"
    );
    assert!(!ENTRY.contains("scope_cell_mut_unpublished"));
    assert!(!ENTRY.contains("proc.scope.write()"));
    assert!(!ENTRY.contains("core::mem::replace"));
}

#[test]
fn process_data_constructor_consumes_only_a_prepared_scope() {
    let init = TASK
        .split_once("pub(crate) struct ProcessDataInit {")
        .expect("process creation must use one named command object")
        .1
        .split_once("pub struct ProcessData {")
        .expect("the creation command must remain a bounded plain-data type")
        .0;
    let constructor = TASK
        .split_once("pub(crate) fn new(")
        .expect("ProcessData construction must stay crate-private")
        .1
        .split_once("/// Takes an owned snapshot")
        .expect("ProcessData construction must remain a bounded section")
        .0;

    assert!(init.contains("prepared_scope: PreparedProcessScope"));
    assert!(constructor.contains("init: ProcessDataInit"));
    assert!(constructor.contains("let ProcessDataInit"));
    assert!(constructor.contains("scope: prepared_scope.into_scope()"));
    assert!(!constructor.contains("scope: ScopeCell::new()"));
}

#[test]
fn prepared_scope_is_complete_before_it_becomes_constructible() {
    let prepare = PROCESS_SCOPE
        .split_once("pub(crate) fn prepare_init()")
        .expect("init scope preparation must be an explicit typed operation")
        .1
        .split_once("/// Assembles inherited FD")
        .expect("init scope preparation must have a bounded body")
        .0;
    let root_ready = prepare
        .find("ensure_root_context_is_published()?")
        .expect("filesystem readiness must be checked as a typed error");
    let scope_create = prepare
        .find("ScopeCell::new()")
        .expect("preparation must own a fresh unpublished scope");
    let fs_install = prepare
        .find("FS_CONTEXT.scope_cell_mut_unpublished")
        .expect("the registered filesystem owner must be initialized off-line");
    let stdio_prepare = prepare
        .find("prepare_stdio_fd_table(&fs_context)?")
        .expect("stdio must be opened against the prepared process context");
    let fd_install = prepare
        .find("FD_TABLE.scope_cell_mut_unpublished")
        .expect("only the completed descriptor table may enter the scope");
    let proof = prepare
        .find("Ok(Self { scope })")
        .expect("only a complete scope may produce the proof object");

    assert!(
        root_ready < scope_create
            && scope_create < fs_install
            && fs_install < stdio_prepare
            && stdio_prepare < fd_install
            && fd_install < proof
    );
    assert!(!prepare.contains("PreemptGuard"));
    assert!(!prepare.contains("scope.write()"));
}

#[test]
fn stdio_preparation_builds_a_plain_table_before_wrapping_it() {
    let preparation = PROCESS_SCOPE
        .split_once("fn prepare_stdio_fd_table(")
        .expect("file support must expose two-phase stdio preparation")
        .1
        .split_once("\n}")
        .expect("stdio preparation must have a bounded function body")
        .0;
    let fs_lock = preparation
        .find("fs_context.lock()")
        .expect("stdio preparation must resolve /dev/console through the filesystem context");
    let plain_table = preparation
        .find("FlattenObjects::new()")
        .expect("descriptors must first be assembled in an unshared plain table");
    let wrap = preparation
        .find("Arc::new(RwLock::new(fd_table))")
        .expect("the completed table must only be wrapped after all opens succeed");

    assert!(
        fs_lock < plain_table && plain_table < wrap,
        "no spin-protected FD table may be held while opening /dev/console"
    );
    assert!(
        !preparation.contains("fd_table.write()"),
        "stdio preparation owns its table exclusively and must not take a spin lock around \
         filesystem I/O"
    );
    assert!(!preparation.contains("current_fs_context"));
}
