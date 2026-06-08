cfg_if::cfg_if! {
    if #[cfg(feature = "fs")] {
        #[cfg(target_os = "none")]
        mod block;

        pub(crate) fn init(bootargs: Option<&str>) {
            #[cfg(target_os = "none")]
            block::init(bootargs);

            #[cfg(not(target_os = "none"))]
            let _ = bootargs;
        }
    } else {
        pub(crate) fn init(_bootargs: Option<&str>) {}
    }
}
