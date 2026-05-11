# 组件概述

TGOSKits 仓库包含 **149** 个 crate，按仓库内直接路径依赖自底向上分为 **16** 层。

## 分类统计

| 分类 | 数 |
|------|------|
| ArceOS 层 | 30 |
| Axvisor 层 | 2 |
| StarryOS 层 | 2 |
| 其他 | 1 |
| 工具层 | 2 |
| 平台层 | 2 |
| 测试层 | 17 |
| 组件层 | 93 |

## 依赖图

`A --> B` 表示 A 依赖 B。

```mermaid
flowchart TB
    subgraph sg_ArceOS__["<b>ArceOS 层</b>"]
        direction TB
        ax_alloc["ax-alloc\nv0.5.0"]
        ax_api["ax-api\nv0.5.0"]
        ax_config["ax-config\nv0.5.0"]
        ax_display["ax-display\nv0.5.0"]
        ax_dma["ax-dma\nv0.5.0"]
        ax_driver["ax-driver\nv0.5.0"]
        ax_feat["ax-feat\nv0.5.0"]
        ax_fs["ax-fs\nv0.5.0"]
        ax_fs_ng["ax-fs-ng\nv0.5.0"]
        ax_hal["ax-hal\nv0.5.0"]
        ax_helloworld["ax-helloworld\nv0.3.0"]
        ax_helloworld_myplat["ax-helloworld-myplat\nv0.3.0"]
        ax_httpclient["ax-httpclient\nv0.3.0"]
        ax_httpserver["ax-httpserver\nv0.3.0"]
        ax_input["ax-input\nv0.5.0"]
        ax_ipi["ax-ipi\nv0.5.0"]
        ax_libc["ax-libc\nv0.5.0"]
        ax_log["ax-log\nv0.5.0"]
        ax_mm["ax-mm\nv0.5.0"]
        ax_net["ax-net\nv0.5.0"]
        ax_net_ng["ax-net-ng\nv0.5.0"]
        ax_posix_api["ax-posix-api\nv0.5.0"]
        ax_runtime["ax-runtime\nv0.5.0"]
        ax_shell["ax-shell\nv0.3.0"]
        ax_std["ax-std\nv0.5.0"]
        ax_sync["ax-sync\nv0.5.0"]
        ax_task["ax-task\nv0.5.0"]
        bwbench_client["bwbench-client\nv0.3.0"]
        deptool["deptool\nv0.3.0"]
        mingo["mingo\nv0.8.0"]
    end
    subgraph sg_Axvisor__["<b>Axvisor 层</b>"]
        direction TB
        ax_plat_riscv64_qemu_virt["ax-plat-riscv64-qemu-virt\nv0.5.0"]
        axvisor["axvisor\nv0.5.0"]
    end
    subgraph sg_StarryOS__["<b>StarryOS 层</b>"]
        direction TB
        starry_kernel["starry-kernel\nv0.4.0"]
        starryos["starryos\nv0.4.0"]
    end
    subgraph sg___["<b>其他</b>"]
        direction TB
        tgmath["tgmath\nv0.3.0"]
    end
    subgraph sg____["<b>工具层</b>"]
        direction TB
        axbuild["axbuild\nv0.4.0"]
        tg_xtask["tg-xtask\nv0.5.0"]
    end
    subgraph sg____["<b>平台层</b>"]
        direction TB
        axplat_dyn["axplat-dyn\nv0.5.0"]
        axplat_x86_qemu_q35["axplat-x86-qemu-q35\nv0.4.0"]
    end
    subgraph sg____["<b>测试层</b>"]
        direction TB
        arceos_affinity["arceos-affinity\nv0.3.0"]
        arceos_display["arceos-display\nv0.3.0"]
        arceos_exception["arceos-exception\nv0.3.0"]
        arceos_fs_shell["arceos-fs-shell\nv0.3.0"]
        arceos_irq["arceos-irq\nv0.3.0"]
        arceos_memtest["arceos-memtest\nv0.3.0"]
        arceos_net_echoserver["arceos-net-echoserver\nv0.3.0"]
        arceos_net_httpclient["arceos-net-httpclient\nv0.3.0"]
        arceos_net_httpserver["arceos-net-httpserver\nv0.3.0"]
        arceos_net_udpserver["arceos-net-udpserver\nv0.3.0"]
        arceos_parallel["arceos-parallel\nv0.3.0"]
        arceos_priority["arceos-priority\nv0.3.0"]
        arceos_sleep["arceos-sleep\nv0.3.0"]
        arceos_tls["arceos-tls\nv0.3.0"]
        arceos_wait_queue["arceos-wait-queue\nv0.3.0"]
        arceos_yield["arceos-yield\nv0.3.0"]
        starryos_test["starryos-test\nv0.5.0"]
    end
    subgraph sg____["<b>组件层</b>"]
        direction TB
        aarch64_sysreg["aarch64_sysreg\nv0.3.1"]
        arm_vcpu["arm_vcpu\nv0.5.0"]
        arm_vgic["arm_vgic\nv0.4.2"]
        ax_allocator["ax-allocator\nv0.4.0"]
        ax_arm_pl011["ax-arm-pl011\nv0.3.0"]
        ax_arm_pl031["ax-arm-pl031\nv0.4.1"]
        ax_cap_access["ax-cap-access\nv0.3.0"]
        ax_config_gen["ax-config-gen\nv0.4.1"]
        ax_config_macros["ax-config-macros\nv0.4.1"]
        ax_cpu["ax-cpu\nv0.5.0"]
        ax_cpumask["ax-cpumask\nv0.3.0"]
        ax_crate_interface["ax-crate-interface\nv0.5.0"]
        ax_crate_interface_lite["ax-crate-interface-lite\nv0.3.0"]
        ax_ctor_bare["ax-ctor-bare\nv0.4.1"]
        ax_ctor_bare_macros["ax-ctor-bare-macros\nv0.4.1"]
        ax_driver_base["ax-driver-base\nv0.3.4"]
        ax_driver_block["ax-driver-block\nv0.3.4"]
        ax_driver_display["ax-driver-display\nv0.3.4"]
        ax_driver_input["ax-driver-input\nv0.3.4"]
        ax_driver_net["ax-driver-net\nv0.3.4"]
        ax_driver_pci["ax-driver-pci\nv0.3.4"]
        ax_driver_virtio["ax-driver-virtio\nv0.3.4"]
        ax_driver_vsock["ax-driver-vsock\nv0.3.4"]
        ax_errno["ax-errno\nv0.4.2"]
        ax_fs_devfs["ax-fs-devfs\nv0.3.2"]
        ax_fs_ramfs["ax-fs-ramfs\nv0.3.2"]
        ax_fs_vfs["ax-fs-vfs\nv0.3.2"]
        ax_handler_table["ax-handler-table\nv0.3.2"]
        ax_int_ratio["ax-int-ratio\nv0.3.2"]
        ax_io["ax-io\nv0.5.0"]
        ax_kernel_guard["ax-kernel-guard\nv0.3.3"]
        ax_kspin["ax-kspin\nv0.3.1"]
        ax_lazyinit["ax-lazyinit\nv0.4.2"]
        ax_linked_list_r4l["ax-linked-list-r4l\nv0.5.0"]
        ax_memory_addr["ax-memory-addr\nv0.6.1"]
        ax_memory_set["ax-memory-set\nv0.6.1"]
        ax_page_table_entry["ax-page-table-entry\nv0.8.1"]
        ax_page_table_multiarch["ax-page-table-multiarch\nv0.8.1"]
        ax_percpu["ax-percpu\nv0.4.3"]
        ax_percpu_macros["ax-percpu-macros\nv0.4.3"]
        ax_plat["ax-plat\nv0.5.1"]
        ax_plat_aarch64_bsta1000b["ax-plat-aarch64-bsta1000b\nv0.5.1"]
        ax_plat_aarch64_peripherals["ax-plat-aarch64-peripherals\nv0.5.1"]
        ax_plat_aarch64_phytium_pi["ax-plat-aarch64-phytium-pi\nv0.5.1"]
        ax_plat_aarch64_qemu_virt["ax-plat-aarch64-qemu-virt\nv0.5.1"]
        ax_plat_aarch64_raspi["ax-plat-aarch64-raspi\nv0.5.1"]
        ax_plat_loongarch64_qemu_virt["ax-plat-loongarch64-qemu-virt\nv0.5.1"]
        ax_plat_macros["ax-plat-macros\nv0.3.0"]
        ax_plat_riscv64_qemu_virt["ax-plat-riscv64-qemu-virt\nv0.5.1"]
        ax_plat_x86_pc["ax-plat-x86-pc\nv0.5.1"]
        ax_riscv_plic["ax-riscv-plic\nv0.4.0"]
        ax_sched["ax-sched\nv0.5.1"]
        ax_timer_list["ax-timer-list\nv0.3.0"]
        axaddrspace["axaddrspace\nv0.5.0"]
        axbacktrace["axbacktrace\nv0.3.2"]
        axdevice["axdevice\nv0.4.2"]
        axdevice_base["axdevice_base\nv0.4.2"]
        axfs_ng_vfs["axfs-ng-vfs\nv0.3.1"]
        axhvc["axhvc\nv0.4.0"]
        axklib["axklib\nv0.5.0"]
        axpoll["axpoll\nv0.3.2"]
        axvcpu["axvcpu\nv0.5.0"]
        axvisor_api["axvisor_api\nv0.5.0"]
        axvisor_api_proc["axvisor_api_proc\nv0.5.0"]
        axvm["axvm\nv0.5.0"]
        axvmconfig["axvmconfig\nv0.4.2"]
        bitmap_allocator["bitmap-allocator\nv0.4.1"]
        cargo_axplat["cargo-axplat\nv0.4.5"]
        define_simple_traits["define-simple-traits\nv0.3.0"]
        define_weak_traits["define-weak-traits\nv0.3.0"]
        fxmac_rs["fxmac_rs\nv0.4.1"]
        hello_kernel["hello-kernel\nv0.3.0"]
        impl_simple_traits["impl-simple-traits\nv0.3.0"]
        impl_weak_partial["impl-weak-partial\nv0.3.0"]
        impl_weak_traits["impl-weak-traits\nv0.3.0"]
        irq_kernel["irq-kernel\nv0.3.0"]
        range_alloc_arceos["range-alloc-arceos\nv0.3.4"]
        riscv_h["riscv-h\nv0.4.0"]
        riscv_vcpu["riscv_vcpu\nv0.5.0"]
        riscv_vplic["riscv_vplic\nv0.4.2"]
        rsext4["rsext4\nv0.3.0"]
        scope_local["scope-local\nv0.3.2"]
        smoltcp["smoltcp\nv0.14.0"]
        smoltcp_fuzz["smoltcp-fuzz\nv0.2.1"]
        smp_kernel["smp-kernel\nv0.3.0"]
        starry_process["starry-process\nv0.4.0"]
        starry_signal["starry-signal\nv0.5.0"]
        starry_vm["starry-vm\nv0.5.0"]
        test_simple["test-simple\nv0.3.0"]
        test_weak["test-weak\nv0.3.0"]
        test_weak_partial["test-weak-partial\nv0.3.0"]
        x86_vcpu["x86_vcpu\nv0.5.0"]
        x86_vlapic["x86_vlapic\nv0.4.2"]
    end
    arceos_affinity --> ax_std
    arceos_display --> ax_std
    arceos_exception --> ax_std
    arceos_fs_shell --> ax_crate_interface
    arceos_fs_shell --> ax_fs_ramfs
    arceos_fs_shell --> ax_fs_vfs
    arceos_fs_shell --> ax_std
    arceos_irq --> ax_std
    arceos_memtest --> ax_std
    arceos_net_echoserver --> ax_std
    arceos_net_httpclient --> ax_std
    arceos_net_httpserver --> ax_std
    arceos_net_udpserver --> ax_std
    arceos_parallel --> ax_std
    arceos_priority --> ax_std
    arceos_sleep --> ax_std
    arceos_tls --> ax_std
    arceos_wait_queue --> ax_std
    arceos_yield --> ax_std
    arm_vcpu --> ax_errno
    arm_vcpu --> ax_percpu
    arm_vcpu --> axaddrspace
    arm_vcpu --> axdevice_base
    arm_vcpu --> axvcpu
    arm_vcpu --> axvisor_api
    arm_vgic --> aarch64_sysreg
    arm_vgic --> ax_errno
    arm_vgic --> ax_memory_addr
    arm_vgic --> axaddrspace
    arm_vgic --> axdevice_base
    arm_vgic --> axvisor_api
    ax_alloc --> ax_allocator
    ax_alloc --> ax_errno
    ax_alloc --> ax_kspin
    ax_alloc --> ax_memory_addr
    ax_alloc --> ax_percpu
    ax_alloc --> axbacktrace
    ax_allocator --> ax_errno
    ax_allocator --> bitmap_allocator
    ax_api --> ax_alloc
    ax_api --> ax_config
    ax_api --> ax_display
    ax_api --> ax_dma
    ax_api --> ax_driver
    ax_api --> ax_errno
    ax_api --> ax_feat
    ax_api --> ax_fs
    ax_api --> ax_hal
    ax_api --> ax_io
    ax_api --> ax_ipi
    ax_api --> ax_log
    ax_api --> ax_mm
    ax_api --> ax_net
    ax_api --> ax_runtime
    ax_api --> ax_sync
    ax_api --> ax_task
    ax_config --> ax_config_macros
    ax_config_macros --> ax_config_gen
    ax_cpu --> ax_lazyinit
    ax_cpu --> ax_memory_addr
    ax_cpu --> ax_page_table_entry
    ax_cpu --> ax_page_table_multiarch
    ax_cpu --> ax_percpu
    ax_cpu --> axbacktrace
    ax_ctor_bare --> ax_ctor_bare_macros
    ax_display --> ax_driver
    ax_display --> ax_lazyinit
    ax_display --> ax_sync
    ax_dma --> ax_alloc
    ax_dma --> ax_allocator
    ax_dma --> ax_config
    ax_dma --> ax_hal
    ax_dma --> ax_kspin
    ax_dma --> ax_memory_addr
    ax_dma --> ax_mm
    ax_driver --> ax_alloc
    ax_driver --> ax_config
    ax_driver --> ax_crate_interface
    ax_driver --> ax_dma
    ax_driver --> ax_driver_base
    ax_driver --> ax_driver_block
    ax_driver --> ax_driver_display
    ax_driver --> ax_driver_input
    ax_driver --> ax_driver_net
    ax_driver --> ax_driver_pci
    ax_driver --> ax_driver_virtio
    ax_driver --> ax_driver_vsock
    ax_driver --> ax_errno
    ax_driver --> ax_hal
    ax_driver --> axplat_dyn
    ax_driver_block --> ax_driver_base
    ax_driver_display --> ax_driver_base
    ax_driver_input --> ax_driver_base
    ax_driver_net --> ax_driver_base
    ax_driver_net --> fxmac_rs
    ax_driver_virtio --> ax_driver_base
    ax_driver_virtio --> ax_driver_block
    ax_driver_virtio --> ax_driver_display
    ax_driver_virtio --> ax_driver_input
    ax_driver_virtio --> ax_driver_net
    ax_driver_virtio --> ax_driver_vsock
    ax_driver_vsock --> ax_driver_base
    ax_feat --> ax_alloc
    ax_feat --> ax_config
    ax_feat --> ax_display
    ax_feat --> ax_driver
    ax_feat --> ax_fs
    ax_feat --> ax_fs_ng
    ax_feat --> ax_hal
    ax_feat --> ax_input
    ax_feat --> ax_ipi
    ax_feat --> ax_kspin
    ax_feat --> ax_log
    ax_feat --> ax_net
    ax_feat --> ax_runtime
    ax_feat --> ax_sync
    ax_feat --> ax_task
    ax_feat --> axbacktrace
    ax_fs --> ax_cap_access
    ax_fs --> ax_driver
    ax_fs --> ax_errno
    ax_fs --> ax_fs_devfs
    ax_fs --> ax_fs_ramfs
    ax_fs --> ax_fs_vfs
    ax_fs --> ax_hal
    ax_fs --> ax_io
    ax_fs --> ax_lazyinit
    ax_fs --> rsext4
    ax_fs_devfs --> ax_fs_vfs
    ax_fs_ng --> ax_alloc
    ax_fs_ng --> ax_driver
    ax_fs_ng --> ax_errno
    ax_fs_ng --> ax_hal
    ax_fs_ng --> ax_io
    ax_fs_ng --> ax_kspin
    ax_fs_ng --> ax_sync
    ax_fs_ng --> axfs_ng_vfs
    ax_fs_ng --> axpoll
    ax_fs_ng --> scope_local
    ax_fs_ramfs --> ax_fs_vfs
    ax_fs_vfs --> ax_errno
    ax_hal --> ax_alloc
    ax_hal --> ax_config
    ax_hal --> ax_cpu
    ax_hal --> ax_kernel_guard
    ax_hal --> ax_memory_addr
    ax_hal --> ax_page_table_multiarch
    ax_hal --> ax_percpu
    ax_hal --> ax_plat
    ax_hal --> ax_plat_aarch64_qemu_virt
    ax_hal --> ax_plat_loongarch64_qemu_virt
    ax_hal --> ax_plat_riscv64_qemu_virt
    ax_hal --> ax_plat_x86_pc
    ax_hal --> axplat_dyn
    ax_helloworld --> ax_std
    ax_helloworld_myplat --> ax_plat_aarch64_bsta1000b
    ax_helloworld_myplat --> ax_plat_aarch64_phytium_pi
    ax_helloworld_myplat --> ax_plat_aarch64_qemu_virt
    ax_helloworld_myplat --> ax_plat_aarch64_raspi
    ax_helloworld_myplat --> ax_plat_loongarch64_qemu_virt
    ax_helloworld_myplat --> ax_plat_riscv64_qemu_virt
    ax_helloworld_myplat --> ax_plat_x86_pc
    ax_helloworld_myplat --> ax_std
    ax_httpclient --> ax_std
    ax_httpserver --> ax_std
    ax_input --> ax_driver
    ax_input --> ax_lazyinit
    ax_input --> ax_sync
    ax_io --> ax_errno
    ax_ipi --> ax_config
    ax_ipi --> ax_hal
    ax_ipi --> ax_kspin
    ax_ipi --> ax_lazyinit
    ax_ipi --> ax_percpu
    ax_kernel_guard --> ax_crate_interface
    ax_kspin --> ax_kernel_guard
    ax_libc --> ax_errno
    ax_libc --> ax_feat
    ax_libc --> ax_io
    ax_libc --> ax_posix_api
    ax_log --> ax_crate_interface
    ax_log --> ax_kspin
    ax_memory_set --> ax_errno
    ax_memory_set --> ax_memory_addr
    ax_mm --> ax_alloc
    ax_mm --> ax_errno
    ax_mm --> ax_hal
    ax_mm --> ax_kspin
    ax_mm --> ax_lazyinit
    ax_mm --> ax_memory_addr
    ax_mm --> ax_memory_set
    ax_mm --> ax_page_table_multiarch
    ax_net --> ax_driver
    ax_net --> ax_errno
    ax_net --> ax_hal
    ax_net --> ax_io
    ax_net --> ax_lazyinit
    ax_net --> ax_sync
    ax_net --> ax_task
    ax_net --> smoltcp
    ax_net_ng --> ax_config
    ax_net_ng --> ax_driver
    ax_net_ng --> ax_errno
    ax_net_ng --> ax_fs_ng
    ax_net_ng --> ax_hal
    ax_net_ng --> ax_io
    ax_net_ng --> ax_sync
    ax_net_ng --> ax_task
    ax_net_ng --> axfs_ng_vfs
    ax_net_ng --> axpoll
    ax_net_ng --> smoltcp
    ax_page_table_entry --> ax_memory_addr
    ax_page_table_multiarch --> ax_errno
    ax_page_table_multiarch --> ax_memory_addr
    ax_page_table_multiarch --> ax_page_table_entry
    ax_percpu --> ax_kernel_guard
    ax_percpu --> ax_percpu_macros
    ax_plat --> ax_crate_interface
    ax_plat --> ax_handler_table
    ax_plat --> ax_kspin
    ax_plat --> ax_memory_addr
    ax_plat --> ax_percpu
    ax_plat --> ax_plat_macros
    ax_plat_aarch64_bsta1000b --> ax_config_macros
    ax_plat_aarch64_bsta1000b --> ax_cpu
    ax_plat_aarch64_bsta1000b --> ax_kspin
    ax_plat_aarch64_bsta1000b --> ax_page_table_entry
    ax_plat_aarch64_bsta1000b --> ax_plat
    ax_plat_aarch64_bsta1000b --> ax_plat_aarch64_peripherals
    ax_plat_aarch64_peripherals --> ax_arm_pl011
    ax_plat_aarch64_peripherals --> ax_arm_pl031
    ax_plat_aarch64_peripherals --> ax_cpu
    ax_plat_aarch64_peripherals --> ax_int_ratio
    ax_plat_aarch64_peripherals --> ax_kspin
    ax_plat_aarch64_peripherals --> ax_lazyinit
    ax_plat_aarch64_peripherals --> ax_plat
    ax_plat_aarch64_phytium_pi --> ax_config_macros
    ax_plat_aarch64_phytium_pi --> ax_cpu
    ax_plat_aarch64_phytium_pi --> ax_page_table_entry
    ax_plat_aarch64_phytium_pi --> ax_plat
    ax_plat_aarch64_phytium_pi --> ax_plat_aarch64_peripherals
    ax_plat_aarch64_qemu_virt --> ax_config_macros
    ax_plat_aarch64_qemu_virt --> ax_cpu
    ax_plat_aarch64_qemu_virt --> ax_page_table_entry
    ax_plat_aarch64_qemu_virt --> ax_plat
    ax_plat_aarch64_qemu_virt --> ax_plat_aarch64_peripherals
    ax_plat_aarch64_raspi --> ax_config_macros
    ax_plat_aarch64_raspi --> ax_cpu
    ax_plat_aarch64_raspi --> ax_page_table_entry
    ax_plat_aarch64_raspi --> ax_plat
    ax_plat_aarch64_raspi --> ax_plat_aarch64_peripherals
    ax_plat_loongarch64_qemu_virt --> ax_config_macros
    ax_plat_loongarch64_qemu_virt --> ax_cpu
    ax_plat_loongarch64_qemu_virt --> ax_kspin
    ax_plat_loongarch64_qemu_virt --> ax_lazyinit
    ax_plat_loongarch64_qemu_virt --> ax_page_table_entry
    ax_plat_loongarch64_qemu_virt --> ax_plat
    ax_plat_macros --> ax_crate_interface
    ax_plat_riscv64_qemu_virt --> ax_config_macros
    ax_plat_riscv64_qemu_virt --> ax_cpu
    ax_plat_riscv64_qemu_virt --> ax_crate_interface
    ax_plat_riscv64_qemu_virt --> ax_kspin
    ax_plat_riscv64_qemu_virt --> ax_lazyinit
    ax_plat_riscv64_qemu_virt --> ax_plat
    ax_plat_riscv64_qemu_virt --> ax_riscv_plic
    ax_plat_riscv64_qemu_virt --> axvisor_api
    ax_plat_x86_pc --> ax_config_macros
    ax_plat_x86_pc --> ax_cpu
    ax_plat_x86_pc --> ax_int_ratio
    ax_plat_x86_pc --> ax_kspin
    ax_plat_x86_pc --> ax_lazyinit
    ax_plat_x86_pc --> ax_percpu
    ax_plat_x86_pc --> ax_plat
    ax_posix_api --> ax_alloc
    ax_posix_api --> ax_config
    ax_posix_api --> ax_errno
    ax_posix_api --> ax_feat
    ax_posix_api --> ax_fs
    ax_posix_api --> ax_hal
    ax_posix_api --> ax_io
    ax_posix_api --> ax_log
    ax_posix_api --> ax_net
    ax_posix_api --> ax_runtime
    ax_posix_api --> ax_sync
    ax_posix_api --> ax_task
    ax_posix_api --> scope_local
    ax_runtime --> ax_alloc
    ax_runtime --> ax_config
    ax_runtime --> ax_crate_interface
    ax_runtime --> ax_ctor_bare
    ax_runtime --> ax_display
    ax_runtime --> ax_driver
    ax_runtime --> ax_fs
    ax_runtime --> ax_fs_ng
    ax_runtime --> ax_hal
    ax_runtime --> ax_input
    ax_runtime --> ax_ipi
    ax_runtime --> ax_log
    ax_runtime --> ax_mm
    ax_runtime --> ax_net
    ax_runtime --> ax_net_ng
    ax_runtime --> ax_percpu
    ax_runtime --> ax_plat
    ax_runtime --> ax_task
    ax_runtime --> axbacktrace
    ax_runtime --> axklib
    ax_sched --> ax_linked_list_r4l
    ax_shell --> ax_std
    ax_std --> ax_api
    ax_std --> ax_errno
    ax_std --> ax_feat
    ax_std --> ax_io
    ax_std --> ax_kspin
    ax_std --> ax_lazyinit
    ax_sync --> ax_kspin
    ax_sync --> ax_task
    ax_task --> ax_config
    ax_task --> ax_cpumask
    ax_task --> ax_crate_interface
    ax_task --> ax_errno
    ax_task --> ax_hal
    ax_task --> ax_kernel_guard
    ax_task --> ax_kspin
    ax_task --> ax_lazyinit
    ax_task --> ax_memory_addr
    ax_task --> ax_percpu
    ax_task --> ax_sched
    ax_task --> ax_timer_list
    ax_task --> axpoll
    axaddrspace --> ax_errno
    axaddrspace --> ax_lazyinit
    axaddrspace --> ax_memory_addr
    axaddrspace --> ax_memory_set
    axaddrspace --> ax_page_table_entry
    axaddrspace --> ax_page_table_multiarch
    axbuild --> axvmconfig
    axdevice --> arm_vgic
    axdevice --> ax_errno
    axdevice --> ax_memory_addr
    axdevice --> axaddrspace
    axdevice --> axdevice_base
    axdevice --> axvmconfig
    axdevice --> range_alloc_arceos
    axdevice --> riscv_vplic
    axdevice_base --> ax_errno
    axdevice_base --> axaddrspace
    axdevice_base --> axvmconfig
    axfs_ng_vfs --> ax_errno
    axfs_ng_vfs --> axpoll
    axhvc --> ax_errno
    axklib --> ax_errno
    axklib --> ax_memory_addr
    axplat_dyn --> ax_alloc
    axplat_dyn --> ax_config_macros
    axplat_dyn --> ax_cpu
    axplat_dyn --> ax_driver_base
    axplat_dyn --> ax_driver_block
    axplat_dyn --> ax_driver_virtio
    axplat_dyn --> ax_errno
    axplat_dyn --> ax_memory_addr
    axplat_dyn --> ax_percpu
    axplat_dyn --> ax_plat
    axplat_dyn --> axklib
    axplat_x86_qemu_q35 --> ax_config_macros
    axplat_x86_qemu_q35 --> ax_cpu
    axplat_x86_qemu_q35 --> ax_int_ratio
    axplat_x86_qemu_q35 --> ax_kspin
    axplat_x86_qemu_q35 --> ax_lazyinit
    axplat_x86_qemu_q35 --> ax_percpu
    axplat_x86_qemu_q35 --> ax_plat
    axvcpu --> ax_errno
    axvcpu --> ax_memory_addr
    axvcpu --> ax_percpu
    axvcpu --> axaddrspace
    axvcpu --> axvisor_api
    axvisor --> ax_config
    axvisor --> ax_cpumask
    axvisor --> ax_crate_interface
    axvisor --> ax_errno
    axvisor --> ax_hal
    axvisor --> ax_kernel_guard
    axvisor --> ax_kspin
    axvisor --> ax_lazyinit
    axvisor --> ax_memory_addr
    axvisor --> ax_page_table_entry
    axvisor --> ax_page_table_multiarch
    axvisor --> ax_percpu
    axvisor --> ax_plat_riscv64_qemu_virt
    axvisor --> ax_std
    axvisor --> ax_timer_list
    axvisor --> axaddrspace
    axvisor --> axbuild
    axvisor --> axdevice
    axvisor --> axdevice_base
    axvisor --> axhvc
    axvisor --> axklib
    axvisor --> axplat_x86_qemu_q35
    axvisor --> axvcpu
    axvisor --> axvisor_api
    axvisor --> axvm
    axvisor --> riscv_vcpu
    axvisor --> riscv_vplic
    axvisor_api --> ax_cpumask
    axvisor_api --> ax_crate_interface
    axvisor_api --> ax_memory_addr
    axvisor_api --> axaddrspace
    axvisor_api --> axvisor_api_proc
    axvm --> arm_vcpu
    axvm --> arm_vgic
    axvm --> ax_cpumask
    axvm --> ax_errno
    axvm --> ax_memory_addr
    axvm --> ax_page_table_entry
    axvm --> ax_page_table_multiarch
    axvm --> ax_percpu
    axvm --> axaddrspace
    axvm --> axdevice
    axvm --> axdevice_base
    axvm --> axvcpu
    axvm --> axvisor_api
    axvm --> axvmconfig
    axvm --> riscv_vcpu
    axvm --> x86_vcpu
    axvmconfig --> ax_errno
    define_simple_traits --> ax_crate_interface
    define_weak_traits --> ax_crate_interface
    fxmac_rs --> ax_crate_interface
    hello_kernel --> ax_plat
    hello_kernel --> ax_plat_aarch64_qemu_virt
    hello_kernel --> ax_plat_loongarch64_qemu_virt
    hello_kernel --> ax_plat_riscv64_qemu_virt
    hello_kernel --> ax_plat_x86_pc
    impl_simple_traits --> ax_crate_interface
    impl_simple_traits --> define_simple_traits
    impl_weak_partial --> ax_crate_interface
    impl_weak_partial --> define_weak_traits
    impl_weak_traits --> ax_crate_interface
    impl_weak_traits --> define_weak_traits
    irq_kernel --> ax_config_macros
    irq_kernel --> ax_cpu
    irq_kernel --> ax_plat
    irq_kernel --> ax_plat_aarch64_qemu_virt
    irq_kernel --> ax_plat_loongarch64_qemu_virt
    irq_kernel --> ax_plat_riscv64_qemu_virt
    irq_kernel --> ax_plat_x86_pc
    riscv_vcpu --> ax_crate_interface
    riscv_vcpu --> ax_errno
    riscv_vcpu --> ax_memory_addr
    riscv_vcpu --> ax_page_table_entry
    riscv_vcpu --> axaddrspace
    riscv_vcpu --> axvcpu
    riscv_vcpu --> axvisor_api
    riscv_vcpu --> riscv_h
    riscv_vplic --> ax_errno
    riscv_vplic --> axaddrspace
    riscv_vplic --> axdevice_base
    riscv_vplic --> axvisor_api
    riscv_vplic --> riscv_h
    scope_local --> ax_percpu
    smoltcp_fuzz --> smoltcp
    smp_kernel --> ax_config_macros
    smp_kernel --> ax_cpu
    smp_kernel --> ax_memory_addr
    smp_kernel --> ax_percpu
    smp_kernel --> ax_plat
    smp_kernel --> ax_plat_aarch64_qemu_virt
    smp_kernel --> ax_plat_loongarch64_qemu_virt
    smp_kernel --> ax_plat_riscv64_qemu_virt
    smp_kernel --> ax_plat_x86_pc
    starry_kernel --> ax_alloc
    starry_kernel --> ax_config
    starry_kernel --> ax_display
    starry_kernel --> ax_driver
    starry_kernel --> ax_errno
    starry_kernel --> ax_feat
    starry_kernel --> ax_fs_ng
    starry_kernel --> ax_hal
    starry_kernel --> ax_input
    starry_kernel --> ax_io
    starry_kernel --> ax_kernel_guard
    starry_kernel --> ax_kspin
    starry_kernel --> ax_log
    starry_kernel --> ax_memory_addr
    starry_kernel --> ax_memory_set
    starry_kernel --> ax_mm
    starry_kernel --> ax_net_ng
    starry_kernel --> ax_page_table_multiarch
    starry_kernel --> ax_percpu
    starry_kernel --> ax_runtime
    starry_kernel --> ax_sync
    starry_kernel --> ax_task
    starry_kernel --> axbacktrace
    starry_kernel --> axfs_ng_vfs
    starry_kernel --> axpoll
    starry_kernel --> scope_local
    starry_kernel --> starry_process
    starry_kernel --> starry_signal
    starry_kernel --> starry_vm
    starry_process --> ax_kspin
    starry_process --> ax_lazyinit
    starry_signal --> ax_cpu
    starry_signal --> ax_kspin
    starry_signal --> starry_vm
    starry_vm --> ax_errno
    starryos --> ax_feat
    starryos --> axbuild
    starryos --> starry_kernel
    starryos_test --> ax_feat
    starryos_test --> starry_kernel
    test_simple --> ax_crate_interface
    test_simple --> define_simple_traits
    test_simple --> impl_simple_traits
    test_weak --> ax_crate_interface
    test_weak --> define_weak_traits
    test_weak --> impl_weak_traits
    test_weak_partial --> ax_crate_interface
    test_weak_partial --> define_weak_traits
    test_weak_partial --> impl_weak_partial
    tg_xtask --> axbuild
    x86_vcpu --> ax_crate_interface
    x86_vcpu --> ax_errno
    x86_vcpu --> ax_memory_addr
    x86_vcpu --> ax_page_table_entry
    x86_vcpu --> axaddrspace
    x86_vcpu --> axdevice_base
    x86_vcpu --> axvcpu
    x86_vcpu --> axvisor_api
    x86_vcpu --> x86_vlapic
    x86_vlapic --> ax_errno
    x86_vlapic --> ax_memory_addr
    x86_vlapic --> axaddrspace
    x86_vlapic --> axdevice_base
    x86_vlapic --> axvisor_api

    classDef cat_comp fill:#e3f2fd,stroke:#1565c0,stroke-width:2px
    classDef cat_arceos fill:#e8f5e9,stroke:#2e7d32,stroke-width:2px
    classDef cat_starry fill:#fce4ec,stroke:#c2185b,stroke-width:2px
    classDef cat_axvisor fill:#e1f5fe,stroke:#01579b,stroke-width:2px
    classDef cat_plat fill:#f3e5f5,stroke:#6a1b9a,stroke-width:2px
    classDef cat_tool fill:#fff8e1,stroke:#f57f17,stroke-width:2px
    classDef cat_test fill:#efebe9,stroke:#5d4037,stroke-width:2px
    classDef cat_misc fill:#eceff1,stroke:#455a64,stroke-width:2px

    class aarch64_sysreg cat_comp
    class arceos_affinity cat_test
    class arceos_display cat_test
    class arceos_exception cat_test
    class arceos_fs_shell cat_test
    class arceos_irq cat_test
    class arceos_memtest cat_test
    class arceos_net_echoserver cat_test
    class arceos_net_httpclient cat_test
    class arceos_net_httpserver cat_test
    class arceos_net_udpserver cat_test
    class arceos_parallel cat_test
    class arceos_priority cat_test
    class arceos_sleep cat_test
    class arceos_tls cat_test
    class arceos_wait_queue cat_test
    class arceos_yield cat_test
    class arm_vcpu cat_comp
    class arm_vgic cat_comp
    class ax_alloc cat_arceos
    class ax_allocator cat_comp
    class ax_api cat_arceos
    class ax_arm_pl011 cat_comp
    class ax_arm_pl031 cat_comp
    class ax_cap_access cat_comp
    class ax_config cat_arceos
    class ax_config_gen cat_comp
    class ax_config_macros cat_comp
    class ax_cpu cat_comp
    class ax_cpumask cat_comp
    class ax_crate_interface cat_comp
    class ax_crate_interface_lite cat_comp
    class ax_ctor_bare cat_comp
    class ax_ctor_bare_macros cat_comp
    class ax_display cat_arceos
    class ax_dma cat_arceos
    class ax_driver cat_arceos
    class ax_driver_base cat_comp
    class ax_driver_block cat_comp
    class ax_driver_display cat_comp
    class ax_driver_input cat_comp
    class ax_driver_net cat_comp
    class ax_driver_pci cat_comp
    class ax_driver_virtio cat_comp
    class ax_driver_vsock cat_comp
    class ax_errno cat_comp
    class ax_feat cat_arceos
    class ax_fs cat_arceos
    class ax_fs_devfs cat_comp
    class ax_fs_ng cat_arceos
    class ax_fs_ramfs cat_comp
    class ax_fs_vfs cat_comp
    class ax_hal cat_arceos
    class ax_handler_table cat_comp
    class ax_helloworld cat_arceos
    class ax_helloworld_myplat cat_arceos
    class ax_httpclient cat_arceos
    class ax_httpserver cat_arceos
    class ax_input cat_arceos
    class ax_int_ratio cat_comp
    class ax_io cat_comp
    class ax_ipi cat_arceos
    class ax_kernel_guard cat_comp
    class ax_kspin cat_comp
    class ax_lazyinit cat_comp
    class ax_libc cat_arceos
    class ax_linked_list_r4l cat_comp
    class ax_log cat_arceos
    class ax_memory_addr cat_comp
    class ax_memory_set cat_comp
    class ax_mm cat_arceos
    class ax_net cat_arceos
    class ax_net_ng cat_arceos
    class ax_page_table_entry cat_comp
    class ax_page_table_multiarch cat_comp
    class ax_percpu cat_comp
    class ax_percpu_macros cat_comp
    class ax_plat cat_comp
    class ax_plat_aarch64_bsta1000b cat_comp
    class ax_plat_aarch64_peripherals cat_comp
    class ax_plat_aarch64_phytium_pi cat_comp
    class ax_plat_aarch64_qemu_virt cat_comp
    class ax_plat_aarch64_raspi cat_comp
    class ax_plat_loongarch64_qemu_virt cat_comp
    class ax_plat_macros cat_comp
    class ax_plat_riscv64_qemu_virt cat_comp
    class ax_plat_riscv64_qemu_virt cat_axvisor
    class ax_plat_x86_pc cat_comp
    class ax_posix_api cat_arceos
    class ax_riscv_plic cat_comp
    class ax_runtime cat_arceos
    class ax_sched cat_comp
    class ax_shell cat_arceos
    class ax_std cat_arceos
    class ax_sync cat_arceos
    class ax_task cat_arceos
    class ax_timer_list cat_comp
    class axaddrspace cat_comp
    class axbacktrace cat_comp
    class axbuild cat_tool
    class axdevice cat_comp
    class axdevice_base cat_comp
    class axfs_ng_vfs cat_comp
    class axhvc cat_comp
    class axklib cat_comp
    class axplat_dyn cat_plat
    class axplat_x86_qemu_q35 cat_plat
    class axpoll cat_comp
    class axvcpu cat_comp
    class axvisor cat_axvisor
    class axvisor_api cat_comp
    class axvisor_api_proc cat_comp
    class axvm cat_comp
    class axvmconfig cat_comp
    class bitmap_allocator cat_comp
    class bwbench_client cat_arceos
    class cargo_axplat cat_comp
    class define_simple_traits cat_comp
    class define_weak_traits cat_comp
    class deptool cat_arceos
    class fxmac_rs cat_comp
    class hello_kernel cat_comp
    class impl_simple_traits cat_comp
    class impl_weak_partial cat_comp
    class impl_weak_traits cat_comp
    class irq_kernel cat_comp
    class mingo cat_arceos
    class range_alloc_arceos cat_comp
    class riscv_h cat_comp
    class riscv_vcpu cat_comp
    class riscv_vplic cat_comp
    class rsext4 cat_comp
    class scope_local cat_comp
    class smoltcp cat_comp
    class smoltcp_fuzz cat_comp
    class smp_kernel cat_comp
    class starry_kernel cat_starry
    class starry_process cat_comp
    class starry_signal cat_comp
    class starry_vm cat_comp
    class starryos cat_starry
    class starryos_test cat_test
    class test_simple cat_comp
    class test_weak cat_comp
    class test_weak_partial cat_comp
    class tg_xtask cat_tool
    class tgmath cat_misc
    class x86_vcpu cat_comp
    class x86_vlapic cat_comp
```


## 外部依赖概要

关系统计来自根目录 **Cargo.lock**，仅统计直接依赖。

| 类别 | 外部包条目数（去重 name+version） |
|------|-------------------------------------|
| 工具库/其他 | 528 |
| 宏/代码生成 | 53 |
| 系统/平台 | 50 |
| 网络/协议 | 29 |
| 异步/并发 | 27 |
| 加密/安全 | 26 |
| 序列化/数据格式 | 24 |
| 日志/错误 | 14 |
| 命令行/配置 | 11 |
| 嵌入式/裸机 | 11 |
| 数据结构/算法 | 10 |
| 设备树/固件 | 8 |

## 相关页面

- [层级关系](layers) — 16 层分级总览与逐 crate 层级表
- [各组件文档](crates) — 149 个 crate 的技术文档索引
