//! Platform policy for AArch64 guest SMC calls.

const ROCKCHIP_SIP_SHARE_MEM: u64 = 0x8200_0009;
const SIP_RET_NOT_SUPPORTED: u64 = (-2_i64) as u64;

pub(super) fn emulate_guest_smc(function: u64, _args: [u64; 3]) -> Option<[u64; 4]> {
    emulate_guest_smc_for_platform(ax_hal::platform_name(), function)
}

fn emulate_guest_smc_for_platform(platform_name: &str, function: u64) -> Option<[u64; 4]> {
    let is_rockchip =
        platform_name.starts_with("Radxa ROCK ") || platform_name.starts_with("Rockchip ");

    (is_rockchip && function == ROCKCHIP_SIP_SHARE_MEM).then_some([SIP_RET_NOT_SUPPORTED, 0, 0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_rockchip_shared_memory_smc() {
        assert_eq!(
            emulate_guest_smc_for_platform("Radxa ROCK 4D SPI", ROCKCHIP_SIP_SHARE_MEM),
            Some([SIP_RET_NOT_SUPPORTED, 0, 0, 0])
        );
    }

    #[test]
    fn leaves_other_platform_smc_calls_to_firmware() {
        assert_eq!(
            emulate_guest_smc_for_platform("linux,dummy-virt", ROCKCHIP_SIP_SHARE_MEM),
            None
        );
        assert_eq!(
            emulate_guest_smc_for_platform("Radxa ROCK 4D SPI", 0x8200_000a),
            None
        );
    }
}
