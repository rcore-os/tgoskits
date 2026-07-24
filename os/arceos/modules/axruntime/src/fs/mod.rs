cfg_if::cfg_if! {
    if #[cfg(feature = "fs")] {
        mod block;

        pub(crate) fn init(bootargs: Option<&str>) {
            block::init(bootargs);
        }

        #[cfg(all(feature = "smp", feature = "ipi"))]
        pub(crate) fn online_smp() {
            block::online_smp();
        }
    } else {
        pub(crate) fn init(_bootargs: Option<&str>) {}

        #[cfg(all(feature = "smp", feature = "ipi"))]
        pub(crate) fn online_smp() {}
    }
}
