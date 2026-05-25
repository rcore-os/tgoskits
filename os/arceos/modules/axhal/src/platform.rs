#[cfg(all(not(test), not(feature = "myplat")))]
include!(concat!(env!("OUT_DIR"), "/selected_platform.rs"));

#[cfg(all(
    target_os = "none",
    not(test),
    not(feature = "myplat"),
    not(feature = "defplat"),
    not(ax_hal_any_platform_feature)
))]
compile_error!("select an ax-hal platform feature or enable ax-hal/myplat");

#[cfg(test)]
#[path = "dummy.rs"]
mod dummy;

#[cfg(all(
    not(test),
    not(target_os = "none"),
    not(feature = "defplat"),
    not(ax_hal_any_platform_feature)
))]
#[path = "dummy.rs"]
mod dummy;

pub mod selected {}
