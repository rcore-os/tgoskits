#[cfg(feature = "rk3588-pwm")]
mod rk3588;
#[cfg(all(feature = "sg2002", not(feature = "rk3588-pwm")))]
mod sg2002;

#[cfg(feature = "rk3588-pwm")]
pub(super) use rk3588::*;
#[cfg(all(feature = "sg2002", not(feature = "rk3588-pwm")))]
pub(super) use sg2002::*;

const NANOS_PER_SECOND: u64 = super::NANOS_PER_SECOND;
