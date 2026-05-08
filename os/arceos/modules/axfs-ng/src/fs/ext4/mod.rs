cfg_if::cfg_if! {
    if #[cfg(feature = "ext4-rsext4")] {
        mod rsext4;
        pub use rsext4::Ext4Filesystem;
    } else if #[cfg(feature = "ext4-lwext4")] {
        mod lwext4;
        pub use lwext4::Ext4Filesystem;
    }
}
