pub(crate) fn enforce(result: ax_cgroup::CgroupResult<()>) {
    if let Err(error) = result {
        // Thread retirement is irreversible at this point. Continuing would
        // leave the cgroup ledger and pids.current permanently charged.
        panic!("cgroup task charge must be released after thread retirement: {error}");
    }
}
