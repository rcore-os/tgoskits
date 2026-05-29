use bitflags::bitflags;
bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    /// flags for BPF_MAP_CREATE command
    pub struct BpfMapCreateFlags: u32 {
        const NO_PREALLOC = 1;
        /* Instead of having one common LRU list in the
         * BPF_MAP_TYPE_LRU_[PERCPU_]HASH map, use a percpu LRU list
         * which can scale and perform better.
         * Note, the LRU nodes (including free nodes) cannot be moved
         * across different LRU lists.
         */
        const NO_COMMON_LRU = 2;
        /* Specify numa node during map creation */
        const NUMA_NODE = 4;
        /* Flags for accessing BPF object from syscall side. */
        const RDONLY = 8;
        const WRONLY = 16;
        /* Flag for stack_map, store build_id+offset instead of pointer */
        const STACK_BUILD_ID = 32;
        /* Zero-initialize hash function seed. This should only be used for testing. */
        const ZERO_SEED = 64;
        /* Flags for accessing BPF object from program side. */
        const RDONLY_PROG = 128;
        const WRONLY_PROG = 256;
        /* Clone map from listener for newly accepted socket */
        const CLONE = 512;
        /* Enable memory-mapping BPF map */
        const MMAPABLE = 1024;
        /* Share perf_event among processes */
        const PRESERVE_ELEMS = 2048;
        /* Create a map that is suitable to be an inner map with dynamic max entries */
        const INNER_MAP = 4096;
        /* Create a map that will be registered/unregesitered by the backed bpf_link */
        const LINK = 8192;
        /* Get path from provided FD in BPF_OBJ_PIN/BPF_OBJ_GET commands */
        const PATH_FD = 16384;
    }
}
