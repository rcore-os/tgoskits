#[cfg(all(not(test), not(feature = "myplat")))]
include!(concat!(env!("OUT_DIR"), "/selected_platform.rs"));

#[cfg(all(
    any(target_os = "none", arceos_std),
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
    not(any(target_os = "none", arceos_std)),
    not(feature = "defplat"),
    not(ax_hal_any_platform_feature)
))]
#[path = "dummy.rs"]
mod dummy;

pub mod selected {}
