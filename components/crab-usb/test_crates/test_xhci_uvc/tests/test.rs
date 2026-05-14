#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate crab_usb;

use bare_test::{
    GetIrqConfig,
    fdt_parser::{PciSpace, Status},
    globals::{PlatformInfoKind, global_val},
    irq::{IrqHandleResult, IrqInfo, IrqParam},
    mem::iomap,
    platform::fdt::GetPciIrqConfig,
    println,
};
use core::time::Duration;
use crab_usb::device::DeviceInfo;
use crab_usb::*;

#[bare_test::tests]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use alloc::{boxed::Box, vec::Vec};

    use bare_test::time::spin_delay;
    use crab_uvc::{UvcDevice, VideoControlEvent, VideoFormatType};
    use ktest_helper::KernelImpl;
    use log::*;
    use pcie::*;

    use super::*;

    static PROT_CHANGED: AtomicBool = AtomicBool::new(false);

    #[test]
    fn test_all() {
        spin_on::spin_on(async {
            let info = get_usb_host();
            let irq_info = info.irq.clone().unwrap();

            let mut host = Box::pin(info.usb);

            register_irq(irq_info, &mut host);

            host.init().await.unwrap();
            info!("usb host init ok");

            let mut devices = Vec::new();
            for _ in 0..50 {
                let ls2 = host.probe_devices().await.unwrap();
                if !ls2.is_empty() {
                    info!("found {} devices", ls2.len());
                    devices = ls2
                        .into_iter()
                        .filter_map(|device| device.into_device_info())
                        .collect();
                    break;
                }
                spin_delay(Duration::from_millis(100));
            }

            info!("usb cmd test");

            let dev_idx = find_camera(devices.iter()).expect("no camera found");

            let dev_info = &devices[dev_idx];
            info!("found camera: {dev_info:?}");

            let dev = host.open_device(dev_info).await.unwrap();

            let mut uvc = UvcDevice::new(dev).await.unwrap();

            // 获取设备信息
            let device_info = uvc.get_device_info().await.unwrap();
            info!("Device info: {}", device_info);

            // 获取支持的视频格式
            let formats = uvc.get_supported_formats().await.unwrap();
            info!("Supported formats:");
            for format in &formats {
                info!("  {:?}", format);
            }

            let format = formats.first().cloned().expect("no format found");

            // for fmt in &formats {
            //     if fmt.width < format.width
            //         && !matches!(fmt.format_type, VideoFormatType::Uncompressed(_))
            //     {
            //         format = fmt.clone();
            //     }
            // }

            uvc.set_format(format.clone()).await.unwrap();

            // 开始视频流
            info!("Starting video streaming...");
            let stream_result = uvc.start_streaming().await;
            let mut stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to start streaming: {:?}", e);
                    // 尝试降级到更低的带宽设置
                    info!("Retrying with different settings...");
                    panic!("Streaming failed: {:?}", e);
                }
            };

            // 获取当前视频格式信息
            let current_format = stream.vedio_format.clone();
            info!("Current video format: {:?}", current_format);

            // 设置一些控制参数的示例
            info!("Setting video controls...");

            // 尝试设置亮度（如果失败也继续）
            if let Err(e) = uvc
                .send_control_command(VideoControlEvent::BrightnessChanged(100))
                .await
            {
                warn!("Failed to set brightness: {:?}", e);
            }

            let mut total_frames = 0;
            let mut first_frame_captured = false;

            // 获取第一帧完整图像后跳出循环
            while !first_frame_captured {
                match stream.recv().await {
                    Ok(frames) => {
                        for frame in frames {
                            total_frames += 1;
                            info!(
                                "Received frame {}: {} bytes",
                                total_frames,
                                frame.data.len()
                            );

                            // 检查是否为完整帧（有EOF标志）
                            if frame.eof && !frame.data.is_empty() {
                                println!("=== CAPTURED FIRST COMPLETE FRAME ===");

                                // 预处理帧数据，移除0值填充
                                let cleaned_data =
                                    if matches!(current_format.format_type, VideoFormatType::Mjpeg)
                                    {
                                        clean_mjpeg_frame_data(&frame.data)
                                    } else {
                                        frame.data.clone()
                                    };

                                info!(
                                    "Frame data cleaned: original {} bytes -> {} bytes",
                                    frame.data.len(),
                                    cleaned_data.len()
                                );

                                // 输出视频格式信息到串口日志
                                println!("VIDEO_FORMAT_START");
                                println!("VIDEO_FORMAT: {:?}", current_format);
                                println!("VIDEO_FORMAT_END");

                                // 输出处理后的图像数据到串口日志（分块输出避免日志缓冲区溢出）
                                println!("FRAME_DATA_START");
                                println!("FRAME_SIZE: {}", cleaned_data.len());
                                println!("ORIGINAL_SIZE: {}", frame.data.len());
                                println!("FRAME_NUMBER: {}", frame.frame_number);
                                if let Some(pts) = frame.pts_90khz {
                                    println!("FRAME_PTS: {}", pts);
                                }

                                // 将数据按4KB块分割输出，跳过全0块
                                const CHUNK_SIZE: usize = 4096 * 4;
                                let chunks = cleaned_data.chunks(CHUNK_SIZE);
                                let total_chunks = chunks.len();
                                let mut output_chunks = 0;

                                for (chunk_idx, chunk) in chunks.enumerate() {
                                    // 跳过全0的块（除了第一块，可能包含重要的头信息）
                                    if chunk_idx > 0 && is_chunk_all_zeros(chunk) {
                                        info!("Skipping all-zero chunk {}", chunk_idx);
                                        continue;
                                    }

                                    // 将每个字节转换为十六进制字符串
                                    let hex_data = chunk
                                        .iter()
                                        .map(|b| alloc::format!("{:02x}", b))
                                        .collect::<Vec<_>>()
                                        .join("");

                                    println!(
                                        "CHUNK_{:04}_{:04}: {}",
                                        output_chunks, total_chunks, hex_data
                                    );
                                    output_chunks += 1;
                                }

                                println!("FRAME_DATA_END");

                                first_frame_captured = true;
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        panic!("Error receiving frame: {:?}", e);
                    }
                }
            }

            info!("First frame captured successfully, stopping stream...");
            // 停止视频流
            drop(stream);
        });
    }

    struct XhciInfo {
        usb: USBHost,
        irq: Option<IrqInfo>,
    }

    fn get_usb_host() -> XhciInfo {
        if let Some(info) = get_usb_host_pcie() {
            return info;
        }

        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;

        let fdt = fdt.get();
        for node in fdt.all_nodes() {
            if matches!(node.status(), Some(Status::Disabled)) {
                continue;
            }

            if node
                .compatibles()
                .any(|c| c.contains("xhci") | c.contains("snps,dwc3"))
            {
                println!("usb node: {}", node.name);
                let regs = node.reg().unwrap().collect::<Vec<_>>();
                println!("usb regs: {:?}", regs);

                let addr = iomap(
                    (regs[0].address as usize).into(),
                    regs[0].size.unwrap_or(0x1000),
                );

                let irq = node.irq_info();

                return XhciInfo {
                    usb: USBHost::new_xhci(addr, &KernelImpl).expect("Failed to create xhci host"),
                    irq,
                };
            }
        }

        panic!("no xhci found");
    }

    fn get_usb_host_pcie() -> Option<XhciInfo> {
        let PlatformInfoKind::DeviceTree(fdt) = &global_val().platform_info;

        let fdt = fdt.get();
        let pcie = fdt
            .find_compatible(&["pci-host-ecam-generic", "brcm,bcm2711-pcie"])
            .next()?;

        let pcie = pcie.into_pci().unwrap();

        let mut pcie_regs = alloc::vec![];

        println!("pcie: {}", pcie.node.name);

        for reg in pcie.node.reg().unwrap() {
            println!(
                "pcie reg: {:#x}, bus: {:#x}",
                reg.address, reg.child_bus_address
            );
            let size = reg.size.unwrap_or_default().align_up(0x1000);

            pcie_regs.push(iomap((reg.address as usize).into(), size));
        }

        let mut bar_alloc = SimpleBarAllocator::default();

        for range in pcie.ranges().unwrap() {
            info!("pcie range: {range:?}");

            match range.space {
                PciSpace::Memory32 => bar_alloc.set_mem32(range.cpu_address as _, range.size as _),
                PciSpace::Memory64 => bar_alloc.set_mem64(range.cpu_address, range.size),
                _ => {}
            }
        }

        let base_vaddr = pcie_regs[0];

        info!("Init PCIE @{base_vaddr:?}");

        let mut root = RootComplexGeneric::new(base_vaddr);

        for elem in root.enumerate(None, Some(bar_alloc)) {
            debug!("PCI {elem}");

            if let Header::Endpoint(mut ep) = elem.header {
                ep.update_command(elem.root, |mut cmd| {
                    cmd.remove(CommandRegister::INTERRUPT_DISABLE);
                    cmd | CommandRegister::IO_ENABLE
                        | CommandRegister::MEMORY_ENABLE
                        | CommandRegister::BUS_MASTER_ENABLE
                });

                for cap in &mut ep.capabilities {
                    match cap {
                        PciCapability::Msi(msi_capability) => {
                            msi_capability.set_enabled(false, &mut *elem.root);
                        }
                        PciCapability::MsiX(msix_capability) => {
                            msix_capability.set_enabled(false, &mut *elem.root);
                        }
                        _ => {}
                    }
                }

                println!("irq_pin {:?}, {:?}", ep.interrupt_pin, ep.interrupt_line);

                if matches!(ep.device_type(), DeviceType::UsbController) {
                    let bar_addr;
                    let mut bar_size;
                    match ep.bar {
                        pcie::BarVec::Memory32(bar_vec_t) => {
                            let bar0 = bar_vec_t[0].as_ref().unwrap();
                            bar_addr = bar0.address as usize;
                            bar_size = bar0.size as usize;
                        }
                        pcie::BarVec::Memory64(bar_vec_t) => {
                            let bar0 = bar_vec_t[0].as_ref().unwrap();
                            bar_addr = bar0.address as usize;
                            bar_size = bar0.size as usize;
                        }
                        pcie::BarVec::Io(_bar_vec_t) => todo!(),
                    };

                    println!("bar0: {:#x}", bar_addr);
                    println!("bar0 size: {:#x}", bar_size);
                    bar_size = bar_size.align_up(0x1000);
                    println!("bar0 size algin: {:#x}", bar_size);

                    let addr = iomap(bar_addr.into(), bar_size);
                    trace!("pin {:?}", ep.interrupt_pin);

                    let irq = pcie.child_irq_info(
                        ep.address.bus(),
                        ep.address.device(),
                        ep.address.function(),
                        ep.interrupt_pin,
                    );

                    println!("irq: {irq:?}");

                    return Some(XhciInfo {
                        usb: USBHost::new_xhci(addr, &KernelImpl)
                            .expect("Failed to create xhci host"),
                        irq,
                    });
                }
            }
        }

        None
    }

    fn register_irq(irq: IrqInfo, host: &mut USBHost) {
        let handle = host.create_event_handler();
        let one = irq.cfgs[0].clone();

        IrqParam {
            intc: irq.irq_parent,
            cfg: one,
        }
        .register_builder({
            move |_irq| {
                let event = handle.handle_event();
                if let Event::PortChange { .. } = event {
                    PROT_CHANGED.store(true, Ordering::Release);
                }

                IrqHandleResult::Handled
            }
        })
        .register();
    }

    fn find_camera<'a>(ls: impl Iterator<Item = &'a DeviceInfo>) -> Option<usize> {
        for (idx, info) in ls.enumerate() {
            if UvcDevice::check(info) {
                return Some(idx);
            }
        }
        None
    }

    /// 清理MJPEG帧数据，移除末尾的0值填充
    fn clean_mjpeg_frame_data(data: &[u8]) -> Vec<u8> {
        // 查找JPEG结束标记 (FFD9)
        for i in 0..data.len().saturating_sub(1) {
            if data[i] == 0xFF && data[i + 1] == 0xD9 {
                // 找到结束标记，截断到此处（包含结束标记）
                return data[..=i + 1].to_vec();
            }
        }

        // 如果没有找到结束标记，移除末尾的大块0值填充
        let mut end_pos = data.len();
        let mut zero_count = 0;
        const MIN_ZERO_BLOCK_SIZE: usize = 1024; // 只移除大于1KB的连续0块

        for i in (0..data.len()).rev() {
            if data[i] == 0 {
                zero_count += 1;
            } else {
                if zero_count >= MIN_ZERO_BLOCK_SIZE {
                    end_pos = i + 1;
                    break;
                }
                zero_count = 0;
            }
        }

        data[..end_pos].to_vec()
    }

    /// 检查数据块是否为全0
    fn is_chunk_all_zeros(chunk: &[u8]) -> bool {
        chunk.iter().all(|&b| b == 0)
    }
}

trait Align {
    fn align_up(&self, align: usize) -> usize;
}

impl Align for usize {
    fn align_up(&self, align: usize) -> usize {
        if (*self).is_multiple_of(align) {
            *self
        } else {
            *self + align - *self % align
        }
    }
}
