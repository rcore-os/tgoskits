//! Source-level contracts for the exec image-commit lock boundary.

const EXECVE: &str = include_str!("../src/syscall/task/execve.rs");
const TASK: &str = include_str!("../src/task/mod.rs");

fn function_body<'source>(source: &'source str, signature: &str, next_item: &str) -> &'source str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"))
        .1
        .split_once(next_item)
        .unwrap_or_else(|| panic!("missing item after {signature}: {next_item}"))
        .0
}

#[test]
fn execve_drops_the_fd_table_guard_before_address_space_commit() {
    let close_transaction = EXECVE
        .find("let mut fd_table = fd_table_owner.write();")
        .expect("execve must remove close-on-exec descriptors in one table transaction");
    let table_unlock = EXECVE[close_transaction..]
        .find("drop(fd_table);")
        .map(|offset| close_transaction + offset)
        .expect("execve must explicitly end the file-table transaction");
    let aspace_commit = EXECVE
        .find("proc_data.replace_aspace(")
        .expect("execve must commit its prepared address space");

    assert!(
        table_unlock < aspace_commit || close_transaction > aspace_commit,
        "a SpinRwLock file-table guard must not span address-space slot release, which may sleep"
    );
}

#[test]
fn address_space_replacement_never_nests_pi_work_under_its_spin_guard() {
    let replacement = function_body(
        TASK,
        "pub fn replace_aspace(",
        "/// Set the vfork completion",
    );
    let attach = replacement
        .find("crate::mm::attach_process_slot(&new_aspace)")
        .expect("the replacement address space must acquire its process slot before publication");
    let spin_lock = replacement
        .find("self.aspace.lock()")
        .expect("address-space publication must use the short spin-protected slot");
    let release = replacement
        .find("crate::mm::release_process_slot(&old)")
        .expect("the retired address space must release its logical process slot");

    assert!(
        attach < spin_lock && spin_lock < release,
        "all PI/sleepable address-space work must remain outside the SpinNoIrq publication window"
    );
    assert_eq!(
        replacement.matches("self.aspace.lock()").count(),
        1,
        "replacement must not re-lock the process slot merely to rediscover the new address space"
    );
}
