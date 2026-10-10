# U-Boot script for explicitly booting a StarryOS FIT from the Linux rootfs.
#
# Linux installs this script as /boot/boot.scr only after the direct serial
# monitor is ready. StarryOS restores the verified Linux backup before starting
# the self-build workload; no U-Boot input is required.

echo "TGOSKits: one-time eMMC StarryOS boot"

setenv env_addr 0x09000000
setenv fit_addr 0x05480000
setenv kernel_load_addr 0x00400000
setenv fdt_load_addr 0x0a100000

if load ${devtype} ${devnum}:${distro_bootpart} ${env_addr} ${prefix}starryEnv.txt; then
    env import -t ${env_addr} ${filesize}
fi
test -n "${starry_fit}" || setenv starry_fit /image.fit

if test -z "${starry_root}"; then
    echo "TGOSKits: starry_root is missing; refusing to guess a Starry block-device index"
else
    setenv bootargs "root=${starry_root} rootwait rw rootfstype=ext4 console=ttyS2,1500000 earlycon=uart8250,mmio32,0xfeb50000"

    if ext4load ${devtype} ${devnum}:${distro_bootpart} ${fit_addr} ${starry_fit}; then
        fdt addr ${fit_addr}
        fdt get addr kernel_data /images/kernel data
        fdt get size kernel_size /images/kernel data
        fdt get addr image_dtb_data /images/fdt data
        fdt get size image_dtb_size /images/fdt data
        cp.b ${kernel_data} ${kernel_load_addr} ${kernel_size}
        cp.b ${image_dtb_data} ${fdt_load_addr} ${image_dtb_size}
        fdt addr ${fdt_load_addr}
        fdt set /chosen bootargs "root=${starry_root} rootwait rw rootfstype=ext4 console=ttyS2,1500000 earlycon=uart8250,mmio32,0xfeb50000"
        fdt print /chosen bootargs
        booti ${kernel_load_addr} - ${fdt_load_addr}
    else
        echo "TGOSKits: failed to load ${starry_fit}"
    fi
fi
