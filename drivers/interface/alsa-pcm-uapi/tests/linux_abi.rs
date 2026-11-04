use core::mem::{align_of, offset_of, size_of};

use alsa_pcm_uapi::*;

#[test]
fn command_numbers_match_64_bit_x86_64_linux() {
    assert_eq!(SNDRV_PCM_VERSION, protocol_version(2, 0, 18));
    assert_eq!(SNDRV_CTL_VERSION, protocol_version(2, 0, 9));
    assert_eq!(SNDRV_PCM_IOCTL_PVERSION, 0x8004_4100);
    assert_eq!(SNDRV_PCM_IOCTL_INFO, 0x8120_4101);
    assert_eq!(SNDRV_PCM_IOCTL_USER_PVERSION, 0x4004_4104);
    assert_eq!(SNDRV_PCM_IOCTL_HW_REFINE, 0xc260_4110);
    assert_eq!(SNDRV_PCM_IOCTL_HW_PARAMS, 0xc260_4111);
    assert_eq!(SNDRV_PCM_IOCTL_HW_FREE, 0x0000_4112);
    assert_eq!(SNDRV_PCM_IOCTL_SW_PARAMS, 0xc088_4113);
    assert_eq!(SNDRV_PCM_IOCTL_STATUS, 0x8098_4120);
    assert_eq!(SNDRV_PCM_IOCTL_DELAY, 0x8008_4121);
    assert_eq!(SNDRV_PCM_IOCTL_HWSYNC, 0x0000_4122);
    assert_eq!(SNDRV_PCM_IOCTL_SYNC_PTR, 0xc088_4123);
    assert_eq!(SNDRV_PCM_IOCTL_SYNC_PTR64, 0xc088_4123);
    assert_eq!(SNDRV_PCM_IOCTL_STATUS_EXT, 0xc098_4124);
    assert_eq!(SNDRV_PCM_IOCTL_CHANNEL_INFO, 0x8018_4132);
    assert_eq!(SNDRV_PCM_IOCTL_PREPARE, 0x0000_4140);
    assert_eq!(SNDRV_PCM_IOCTL_START, 0x0000_4142);
    assert_eq!(SNDRV_PCM_IOCTL_WRITEI_FRAMES, 0x4018_4150);
    assert_eq!(SNDRV_PCM_IOCTL_READI_FRAMES, 0x8018_4151);
    assert_eq!(SNDRV_CTL_IOCTL_PVERSION, 0x8004_5500);
    assert_eq!(SNDRV_CTL_IOCTL_CARD_INFO, 0x8178_5501);
    assert_eq!(SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE, 0x8004_5530);
    assert_eq!(SNDRV_CTL_IOCTL_PCM_INFO, 0xc120_5531);
}

#[test]
fn pcm_and_control_layouts_match_64_bit_linux() {
    assert_eq!(size_of::<LinuxTimespec>(), 16);
    assert_eq!(size_of::<PcmMask>(), 32);
    assert_eq!(align_of::<PcmMask>(), 4);
    assert_eq!(size_of::<PcmInterval>(), 12);
    assert_eq!(align_of::<PcmInterval>(), 4);
    assert_eq!(size_of::<PcmInfo>(), 288);
    assert_eq!(offset_of!(PcmInfo, pad1), 208);
    assert_eq!(offset_of!(PcmInfo, reserved), 224);
    assert_eq!(size_of::<CtlCardInfo>(), 376);
    assert_eq!(offset_of!(CtlCardInfo, longname), 72);
    assert_eq!(offset_of!(CtlCardInfo, mixername), 168);
    assert_eq!(offset_of!(CtlCardInfo, components), 248);
    assert_eq!(size_of::<PcmHwParams>(), 608);
    assert_eq!(offset_of!(PcmHwParams, masks), 4);
    assert_eq!(offset_of!(PcmHwParams, mres), 100);
    assert_eq!(offset_of!(PcmHwParams, intervals), 260);
    assert_eq!(offset_of!(PcmHwParams, ires), 404);
    assert_eq!(offset_of!(PcmHwParams, rmask), 512);
    assert_eq!(offset_of!(PcmHwParams, fifo_size), 536);
    assert_eq!(offset_of!(PcmHwParams, sync), 544);
    assert_eq!(offset_of!(PcmHwParams, reserved), 560);
    assert_eq!(size_of::<PcmSwParams>(), 136);
    assert_eq!(offset_of!(PcmSwParams, avail_min), 16);
    assert_eq!(offset_of!(PcmSwParams, boundary), 64);
    assert_eq!(offset_of!(PcmSwParams, proto), 72);
    assert_eq!(offset_of!(PcmSwParams, reserved), 80);
    assert_eq!(size_of::<PcmStatus>(), 152);
    assert_eq!(offset_of!(PcmStatus, trigger_tstamp), 8);
    assert_eq!(offset_of!(PcmStatus, appl_ptr), 40);
    assert_eq!(offset_of!(PcmStatus, audio_tstamp), 96);
    assert_eq!(offset_of!(PcmStatus, driver_tstamp), 112);
    assert_eq!(offset_of!(PcmStatus, reserved), 132);
    assert_eq!(size_of::<PcmMmapStatus>(), 56);
    assert_eq!(offset_of!(PcmMmapStatus, hw_ptr), 8);
    assert_eq!(offset_of!(PcmMmapStatus, tstamp), 16);
    assert_eq!(offset_of!(PcmMmapStatus, suspended_state), 32);
    assert_eq!(offset_of!(PcmMmapStatus, audio_tstamp), 40);
    assert_eq!(size_of::<PcmMmapStatus64>(), 56);
    assert_eq!(offset_of!(PcmMmapStatus64, hw_ptr), 8);
    assert_eq!(offset_of!(PcmMmapStatus64, tstamp), 16);
    assert_eq!(offset_of!(PcmMmapStatus64, suspended_state), 32);
    assert_eq!(offset_of!(PcmMmapStatus64, audio_tstamp), 40);
    assert_eq!(size_of::<PcmMmapControl>(), 16);
    assert_eq!(size_of::<PcmMmapControl64>(), 16);
    assert_eq!(size_of::<PcmSyncStatusStorage>(), 64);
    assert_eq!(size_of::<PcmSyncControlStorage>(), 64);
    assert_eq!(size_of::<PcmSyncStatusStorage64>(), 64);
    assert_eq!(size_of::<PcmSyncControlStorage64>(), 64);
    assert_eq!(size_of::<PcmSyncPtr>(), 136);
    assert_eq!(offset_of!(PcmSyncPtr, s), 8);
    assert_eq!(offset_of!(PcmSyncPtr, c), 72);
    assert_eq!(size_of::<PcmSyncPtr64>(), 136);
    assert_eq!(offset_of!(PcmSyncPtr64, s), 8);
    assert_eq!(offset_of!(PcmSyncPtr64, c), 72);
    assert_eq!(size_of::<PcmChannelInfo>(), 24);
    assert_eq!(offset_of!(PcmChannelInfo, offset), 8);
    assert_eq!(offset_of!(PcmChannelInfo, first), 16);
    assert_eq!(offset_of!(PcmChannelInfo, step), 20);
    assert_eq!(size_of::<PcmXferi>(), 24);
    assert_eq!(offset_of!(PcmXferi, buf), 8);
    assert_eq!(offset_of!(PcmXferi, frames), 16);
    assert_eq!(size_of::<PcmXfern>(), 24);
    assert_eq!(offset_of!(PcmXfern, bufs), 8);
    assert_eq!(offset_of!(PcmXfern, frames), 16);
}
