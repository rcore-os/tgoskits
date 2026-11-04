/*
 * Compile with -std=c11 against the selected Linux sound/asound.h and x86_64
 * headers. It only prints the compiler's ABI view; it does not exercise a PCM
 * device.
 */
#include <stddef.h>
#include <stdio.h>
#include <sys/ioctl.h>

#include <sound/asound.h>

#define PRINT_SIZE(tag, type) \
	printf("%-32s size=%zu align=%zu\n", tag, sizeof(type), _Alignof(type))

#define PRINT_OFFSET(tag, type, field) \
	printf("%-32s offsetof(%s)=%zu\n", tag, #field, offsetof(type, field))

#define PRINT_IOCTL(tag, command) \
	printf("%-32s ioctl=0x%lx\n", tag, (unsigned long)(command))

int main(void)
{
	printf("%-32s version=0x%x\n", "SNDRV_PCM_VERSION", SNDRV_PCM_VERSION);
	printf("%-32s version=0x%x\n", "SNDRV_CTL_VERSION", SNDRV_CTL_VERSION);

	PRINT_SIZE("snd_pcm_info", struct snd_pcm_info);
	PRINT_OFFSET("snd_pcm_info", struct snd_pcm_info, pad1);
	PRINT_OFFSET("snd_pcm_info", struct snd_pcm_info, reserved);

	PRINT_SIZE("snd_ctl_card_info", struct snd_ctl_card_info);
	PRINT_OFFSET("snd_ctl_card_info", struct snd_ctl_card_info, longname);
	PRINT_OFFSET("snd_ctl_card_info", struct snd_ctl_card_info, mixername);
	PRINT_OFFSET("snd_ctl_card_info", struct snd_ctl_card_info, components);

	PRINT_SIZE("snd_mask", struct snd_mask);
	PRINT_SIZE("snd_interval", struct snd_interval);
	PRINT_SIZE("snd_pcm_hw_params", struct snd_pcm_hw_params);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, masks);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, mres);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, intervals);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, ires);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, rmask);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, fifo_size);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, sync);
	PRINT_OFFSET("snd_pcm_hw_params", struct snd_pcm_hw_params, reserved);

	PRINT_SIZE("snd_pcm_sw_params", struct snd_pcm_sw_params);
	PRINT_OFFSET("snd_pcm_sw_params", struct snd_pcm_sw_params, avail_min);
	PRINT_OFFSET("snd_pcm_sw_params", struct snd_pcm_sw_params, boundary);
	PRINT_OFFSET("snd_pcm_sw_params", struct snd_pcm_sw_params, proto);
	PRINT_OFFSET("snd_pcm_sw_params", struct snd_pcm_sw_params, reserved);

	PRINT_SIZE("snd_pcm_status", struct snd_pcm_status);
	PRINT_OFFSET("snd_pcm_status", struct snd_pcm_status, trigger_tstamp);
	PRINT_OFFSET("snd_pcm_status", struct snd_pcm_status, appl_ptr);
	PRINT_OFFSET("snd_pcm_status", struct snd_pcm_status, audio_tstamp);
	PRINT_OFFSET("snd_pcm_status", struct snd_pcm_status, driver_tstamp);
	PRINT_OFFSET("snd_pcm_status", struct snd_pcm_status, reserved);

	PRINT_SIZE("snd_pcm_mmap_status", struct snd_pcm_mmap_status);
	PRINT_OFFSET("snd_pcm_mmap_status", struct snd_pcm_mmap_status, hw_ptr);
	PRINT_OFFSET("snd_pcm_mmap_status", struct snd_pcm_mmap_status, tstamp);
	PRINT_OFFSET("snd_pcm_mmap_status", struct snd_pcm_mmap_status, audio_tstamp);

	PRINT_SIZE("snd_pcm_mmap_status64", struct __snd_pcm_mmap_status64);
	PRINT_OFFSET("snd_pcm_mmap_status64", struct __snd_pcm_mmap_status64, hw_ptr);
	PRINT_OFFSET("snd_pcm_mmap_status64", struct __snd_pcm_mmap_status64, tstamp);
	PRINT_OFFSET("snd_pcm_mmap_status64", struct __snd_pcm_mmap_status64, audio_tstamp);

	PRINT_SIZE("snd_pcm_mmap_control", struct snd_pcm_mmap_control);
	PRINT_SIZE("snd_pcm_mmap_control64", struct __snd_pcm_mmap_control64);
	PRINT_SIZE("snd_pcm_channel_info", struct snd_pcm_channel_info);
	PRINT_OFFSET("snd_pcm_channel_info", struct snd_pcm_channel_info, offset);
	PRINT_SIZE("snd_pcm_sync_ptr", struct snd_pcm_sync_ptr);
	PRINT_OFFSET("snd_pcm_sync_ptr", struct snd_pcm_sync_ptr, s);
	PRINT_OFFSET("snd_pcm_sync_ptr", struct snd_pcm_sync_ptr, c);
	PRINT_SIZE("snd_pcm_sync_ptr64", struct __snd_pcm_sync_ptr64);
	PRINT_OFFSET("snd_pcm_sync_ptr64", struct __snd_pcm_sync_ptr64, s);
	PRINT_OFFSET("snd_pcm_sync_ptr64", struct __snd_pcm_sync_ptr64, c);

	PRINT_SIZE("snd_xferi", struct snd_xferi);
	PRINT_OFFSET("snd_xferi", struct snd_xferi, buf);
	PRINT_OFFSET("snd_xferi", struct snd_xferi, frames);

	PRINT_IOCTL("SNDRV_PCM_IOCTL_PVERSION", SNDRV_PCM_IOCTL_PVERSION);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_INFO", SNDRV_PCM_IOCTL_INFO);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_HW_REFINE", SNDRV_PCM_IOCTL_HW_REFINE);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_HW_PARAMS", SNDRV_PCM_IOCTL_HW_PARAMS);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_SW_PARAMS", SNDRV_PCM_IOCTL_SW_PARAMS);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_STATUS", SNDRV_PCM_IOCTL_STATUS);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_STATUS_EXT", SNDRV_PCM_IOCTL_STATUS_EXT);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_CHANNEL_INFO", SNDRV_PCM_IOCTL_CHANNEL_INFO);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_SYNC_PTR", SNDRV_PCM_IOCTL_SYNC_PTR);
	PRINT_IOCTL("__SNDRV_PCM_IOCTL_SYNC_PTR", __SNDRV_PCM_IOCTL_SYNC_PTR);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_SYNC_PTR64", __SNDRV_PCM_IOCTL_SYNC_PTR64);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_WRITEI_FRAMES", SNDRV_PCM_IOCTL_WRITEI_FRAMES);
	PRINT_IOCTL("SNDRV_PCM_IOCTL_READI_FRAMES", SNDRV_PCM_IOCTL_READI_FRAMES);
	PRINT_IOCTL("SNDRV_CTL_IOCTL_CARD_INFO", SNDRV_CTL_IOCTL_CARD_INFO);
	PRINT_IOCTL("SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE", SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE);
	PRINT_IOCTL("SNDRV_CTL_IOCTL_PCM_INFO", SNDRV_CTL_IOCTL_PCM_INFO);

	return 0;
}
