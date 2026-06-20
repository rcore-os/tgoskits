cfg_if::cfg_if! {
    if #[cfg(feature = "fs")] {
        mod block;

        pub(crate) fn init(bootargs: Option<&str>) {
            block::init(bootargs);
        }
    } else {
        pub(crate) fn init(_bootargs: Option<&str>) {}
    }
}
