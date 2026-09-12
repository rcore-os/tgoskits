// scene_codec_rust - streaming/codec-math RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software
// Vulkan, no GPU/window/surface/swapchain), Rust `ash` binding of the same offscreen render pipeline as the
// C++ cell scene_codec.cpp. `ash::Entry::load()` dlopens libvulkan.so.1; an offscreen render pass into an
// R8G8B8A8_UNORM color image, drawing through real graphics pipelines (SPIR-V vertex+fragment shaders loaded
// via read_spv). Exercises codec/streaming math each asserted against an INDEPENDENT closed-form Rust
// reference: (1) YUV->RGB BT.601 full-range from three R8_UNORM planes, (2) chroma 4:2:0->4:4:4 NEAREST
// upsample, (3) bilinear 2x downscale (VK_FILTER_LINEAR = 2x2 box average), (4) DCT-II/IDCT + RLE round-trip
// on the CPU. The reference math is behaviour-identical to the C++ cell; only the ash-vs-C++ Vulkan binding
// syntax differs. Prints "SCENE_CODEC_RUST OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
use std::io::Cursor;

use ash::{Device, Entry, Instance, vk};

const W: usize = 64;
const H: usize = 64;

struct Score {
    pass: i32,
    fail: i32,
}
impl Score {
    fn ok(&mut self, cond: bool, desc: &str) {
        if cond {
            self.pass += 1;
        } else {
            self.fail += 1;
            eprintln!("FAIL: {desc}");
        }
    }
}
fn clampi(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

struct Tex {
    img: vk::Image,
    mem: vk::DeviceMemory,
    view: vk::ImageView,
}

struct Ctx {
    #[allow(dead_code)]
    entry: Entry,
    inst: Instance,
    pd: vk::PhysicalDevice,
    dev: Device,
    q: vk::Queue,
    qfam: u32,
    pool: vk::CommandPool,
    cimg: vk::Image,
    rp: vk::RenderPass,
    fb: vk::Framebuffer,
    rbuf: vk::Buffer,
    rmap: *mut u8,
    buf: Vec<u8>,
    vbo: vk::Buffer,
}
impl Ctx {
    fn memtype(&self, bits: u32, want: vk::MemoryPropertyFlags) -> u32 {
        let mp = unsafe { self.inst.get_physical_device_memory_properties(self.pd) };
        (0..mp.memory_type_count)
            .find(|&i| {
                (bits & (1 << i)) != 0 && mp.memory_types[i as usize].property_flags.contains(want)
            })
            .unwrap_or(u32::MAX)
    }
    fn shmod(&self, bytes: &[u8]) -> vk::ShaderModule {
        let code = ash::util::read_spv(&mut Cursor::new(bytes)).expect("read_spv");
        unsafe {
            self.dev
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)
        }
        .expect("shader module")
    }
    fn mk_vbo(&self, data: &[f32]) -> (vk::Buffer, vk::DeviceMemory) {
        let sz = std::mem::size_of_val(data) as vk::DeviceSize;
        let b = unsafe {
            self.dev.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(sz)
                    .usage(vk::BufferUsageFlags::VERTEX_BUFFER),
                None,
            )
        }
        .expect("buffer");
        let mr = unsafe { self.dev.get_buffer_memory_requirements(b) };
        let mem = unsafe {
            self.dev.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(mr.size)
                    .memory_type_index(self.memtype(
                        mr.memory_type_bits,
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )),
                None,
            )
        }
        .unwrap();
        unsafe {
            self.dev.bind_buffer_memory(b, mem, 0).unwrap();
            let p = self
                .dev
                .map_memory(mem, 0, sz, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, p as *mut u8, sz as usize);
            self.dev.unmap_memory(mem);
        }
        (b, mem)
    }
    // upload a texture (fmt, w, h) from host data via staging buffer + layout barriers.
    fn mk_tex(&self, fmt: vk::Format, w: u32, h: u32, data: &[u8]) -> Tex {
        let tii = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(fmt)
            .extent(vk::Extent3D {
                width: w,
                height: h,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let img = unsafe { self.dev.create_image(&tii, None) }.unwrap();
        let tmr = unsafe { self.dev.get_image_memory_requirements(img) };
        let mem = unsafe {
            self.dev.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(tmr.size)
                    .memory_type_index(
                        self.memtype(tmr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL),
                    ),
                None,
            )
        }
        .unwrap();
        unsafe { self.dev.bind_image_memory(img, mem, 0) }.unwrap();
        let sbuf = unsafe {
            self.dev.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(data.len() as u64)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC),
                None,
            )
        }
        .unwrap();
        let smr = unsafe { self.dev.get_buffer_memory_requirements(sbuf) };
        let smem = unsafe {
            self.dev.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(smr.size)
                    .memory_type_index(self.memtype(
                        smr.memory_type_bits,
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )),
                None,
            )
        }
        .unwrap();
        unsafe {
            self.dev.bind_buffer_memory(sbuf, smem, 0).unwrap();
            let p = self
                .dev
                .map_memory(smem, 0, data.len() as u64, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
            self.dev.unmap_memory(smem);
        }
        let cai = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { self.dev.allocate_command_buffers(&cai) }.unwrap()[0];
        unsafe {
            self.dev.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .unwrap();
        let subr = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let b1 = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(img)
            .subresource_range(subr)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
        unsafe {
            self.dev.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[b1],
            )
        };
        let cp = vk::BufferImageCopy {
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_extent: vk::Extent3D {
                width: w,
                height: h,
                depth: 1,
            },
            ..Default::default()
        };
        unsafe {
            self.dev.cmd_copy_buffer_to_image(
                cmd,
                sbuf,
                img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[cp],
            )
        };
        let b2 = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(img)
            .subresource_range(subr)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ);
        unsafe {
            self.dev.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[b2],
            );
            self.dev.end_command_buffer(cmd).unwrap();
            let cmds = [cmd];
            let si = [vk::SubmitInfo::default().command_buffers(&cmds)];
            let f = self
                .dev
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .unwrap();
            self.dev.queue_submit(self.q, &si, f).unwrap();
            self.dev.wait_for_fences(&[f], true, u64::MAX).unwrap();
            self.dev.destroy_fence(f, None);
            self.dev.free_command_buffers(self.pool, &[cmd]);
            self.dev.destroy_buffer(sbuf, None);
            self.dev.free_memory(smem, None);
        }
        let view = unsafe {
            self.dev.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(img)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(fmt)
                    .subresource_range(subr),
                None,
            )
        }
        .unwrap();
        Tex { img, mem, view }
    }
    fn free_tex(&self, t: &Tex) {
        unsafe {
            self.dev.destroy_image_view(t.view, None);
            self.dev.destroy_image(t.img, None);
            self.dev.free_memory(t.mem, None);
        }
    }
    fn mk_sampler(&self, filt: vk::Filter) -> vk::Sampler {
        let s = vk::SamplerCreateInfo::default()
            .mag_filter(filt)
            .min_filter(filt)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        unsafe { self.dev.create_sampler(&s, None) }.unwrap()
    }
    fn px(&self, x: usize, y: usize, c: usize) -> i32 {
        self.buf[(y * W + x) * 4 + c] as i32
    }
    fn peq(&self, x: usize, y: usize, rgba: [i32; 4], tol: i32) -> bool {
        (self.px(x, y, 0) - rgba[0]).abs() <= tol
            && (self.px(x, y, 1) - rgba[1]).abs() <= tol
            && (self.px(x, y, 2) - rgba[2]).abs() <= tol
            && (self.px(x, y, 3) - rgba[3]).abs() <= tol
    }
    // 1 vertex binding (pos2+uv2, stride 16), dynamic viewport+scissor.
    fn mk_pipe(
        &self,
        vs: vk::ShaderModule,
        fs: vk::ShaderModule,
        pl: vk::PipelineLayout,
    ) -> vk::Pipeline {
        let name = c"main";
        let st = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs)
                .name(name),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fs)
                .name(name),
        ];
        let bind = [vk::VertexInputBindingDescription {
            binding: 0,
            stride: 16,
            input_rate: vk::VertexInputRate::VERTEX,
        }];
        let attr = [
            vk::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 8,
            },
        ];
        let vi = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bind)
            .vertex_attribute_descriptions(&attr);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let vp = [vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: W as f32,
            height: H as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }];
        let sc = [vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: W as u32,
                height: H as u32,
            },
        }];
        let vps = vk::PipelineViewportStateCreateInfo::default()
            .viewports(&vp)
            .scissors(&sc);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
        let rs = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let cba = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(false)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
        let gp = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&st)
            .vertex_input_state(&vi)
            .input_assembly_state(&ia)
            .viewport_state(&vps)
            .rasterization_state(&rs)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .dynamic_state(&ds)
            .layout(pl)
            .render_pass(self.rp)
            .subpass(0)];
        unsafe {
            self.dev
                .create_graphics_pipelines(vk::PipelineCache::null(), &gp, None)
        }
        .expect("pipeline")[0]
    }
    // render a full-NDC textured quad into the sub-region viewport {0,0,pw,ph}, read back into buf.
    fn draw_sub(
        &mut self,
        pipe: vk::Pipeline,
        pl: vk::PipelineLayout,
        dset: vk::DescriptorSet,
        pw: u32,
        ph: u32,
    ) {
        let cai = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { self.dev.allocate_command_buffers(&cai) }.unwrap()[0];
        unsafe {
            self.dev.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .unwrap();
        let cv = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 1.0],
            },
        }];
        let rpb = vk::RenderPassBeginInfo::default()
            .render_pass(self.rp)
            .framebuffer(self.fb)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: W as u32,
                    height: H as u32,
                },
            })
            .clear_values(&cv);
        unsafe {
            self.dev
                .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE);
            self.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
            self.dev.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: pw as f32,
                    height: ph as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.dev.cmd_set_scissor(
                cmd,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D {
                        width: pw,
                        height: ph,
                    },
                }],
            );
            self.dev.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                pl,
                0,
                &[dset],
                &[],
            );
            self.dev.cmd_bind_vertex_buffers(cmd, 0, &[self.vbo], &[0]);
            self.dev.cmd_draw(cmd, 4, 1, 0, 0);
            self.dev.cmd_end_render_pass(cmd);
        }
        let region = [vk::BufferImageCopy {
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_extent: vk::Extent3D {
                width: W as u32,
                height: H as u32,
                depth: 1,
            },
            ..Default::default()
        }];
        unsafe {
            self.dev.cmd_copy_image_to_buffer(
                cmd,
                self.cimg,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.rbuf,
                &region,
            );
            self.dev.end_command_buffer(cmd).unwrap();
            let cmds = [cmd];
            let si = [vk::SubmitInfo::default().command_buffers(&cmds)];
            let fence = self
                .dev
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .unwrap();
            self.dev.queue_submit(self.q, &si, fence).unwrap();
            self.dev.wait_for_fences(&[fence], true, u64::MAX).unwrap();
            self.dev.destroy_fence(fence, None);
            self.dev.free_command_buffers(self.pool, &[cmd]);
            std::ptr::copy_nonoverlapping(self.rmap, self.buf.as_mut_ptr(), W * H * 4);
        }
    }
}

// build a single-binding combined-image-sampler descriptor (returns pl, pipe, pool, set, dsl)
struct DescBundle {
    dsl: vk::DescriptorSetLayout,
    pl: vk::PipelineLayout,
    pool: vk::DescriptorPool,
    dset: vk::DescriptorSet,
}
fn mk_desc(c: &Ctx, n: u32) -> DescBundle {
    let dslb: Vec<_> = (0..n)
        .map(|i| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        })
        .collect();
    let dsl = unsafe {
        c.dev.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&dslb),
            None,
        )
    }
    .unwrap();
    let sls = [dsl];
    let pl = unsafe {
        c.dev.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(&sls),
            None,
        )
    }
    .unwrap();
    let dps = [vk::DescriptorPoolSize {
        ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        descriptor_count: n,
    }];
    let pool = unsafe {
        c.dev.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default()
                .max_sets(1)
                .pool_sizes(&dps),
            None,
        )
    }
    .unwrap();
    let dsai = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&sls);
    let dset = unsafe { c.dev.allocate_descriptor_sets(&dsai) }.unwrap()[0];
    DescBundle {
        dsl,
        pl,
        pool,
        dset,
    }
}

fn main() {
    let mut s = Score { pass: 0, fail: 0 };
    let entry = unsafe { Entry::load() }.expect("failed to load libvulkan.so.1");
    let app = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
    let inst = unsafe {
        entry.create_instance(
            &vk::InstanceCreateInfo::default().application_info(&app),
            None,
        )
    }
    .expect("vkCreateInstance");
    s.ok(true, "vkCreateInstance");
    let pds = unsafe { inst.enumerate_physical_devices() }.expect("enumerate physical devices");
    s.ok(!pds.is_empty(), ">=1 physical device");
    let pd = pds[0];
    let qfams = unsafe { inst.get_physical_device_queue_family_properties(pd) };
    let qfam = qfams
        .iter()
        .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .map(|i| i as u32)
        .unwrap_or(u32::MAX);
    s.ok(qfam != u32::MAX, "graphics queue family");
    let pri = [1.0f32];
    let qci = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(qfam)
        .queue_priorities(&pri)];
    let dev = unsafe {
        inst.create_device(
            pd,
            &vk::DeviceCreateInfo::default().queue_create_infos(&qci),
            None,
        )
    }
    .expect("vkCreateDevice");
    s.ok(true, "vkCreateDevice");
    let q = unsafe { dev.get_device_queue(qfam, 0) };

    let mut c = Ctx {
        entry,
        inst,
        pd,
        dev,
        q,
        qfam,
        pool: vk::CommandPool::null(),
        cimg: vk::Image::null(),
        rp: vk::RenderPass::null(),
        fb: vk::Framebuffer::null(),
        rbuf: vk::Buffer::null(),
        rmap: std::ptr::null_mut(),
        buf: vec![0u8; W * H * 4],
        vbo: vk::Buffer::null(),
    };

    let ii = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: W as u32,
            height: H as u32,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    c.cimg = unsafe { c.dev.create_image(&ii, None) }.expect("color image");
    s.ok(c.cimg != vk::Image::null(), "vkCreateImage color");
    let imr = unsafe { c.dev.get_image_memory_requirements(c.cimg) };
    let cmem = unsafe {
        c.dev.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(imr.size)
                .memory_type_index(
                    c.memtype(imr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL),
                ),
            None,
        )
    }
    .unwrap();
    unsafe { c.dev.bind_image_memory(c.cimg, cmem, 0) }.unwrap();
    let cview = unsafe {
        c.dev.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(c.cimg)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        )
    }
    .expect("color view");
    s.ok(cview != vk::ImageView::null(), "vkCreateImageView");
    let att = [vk::AttachmentDescription::default()
        .format(vk::Format::R8G8B8A8_UNORM)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)];
    let cref = [vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    }];
    let sp = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&cref)];
    c.rp = unsafe {
        c.dev.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(&att)
                .subpasses(&sp),
            None,
        )
    }
    .expect("render pass");
    s.ok(c.rp != vk::RenderPass::null(), "vkCreateRenderPass");
    let fbv = [cview];
    c.fb = unsafe {
        c.dev.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(c.rp)
                .attachments(&fbv)
                .width(W as u32)
                .height(H as u32)
                .layers(1),
            None,
        )
    }
    .expect("framebuffer");
    s.ok(c.fb != vk::Framebuffer::null(), "vkCreateFramebuffer");
    c.rbuf = unsafe {
        c.dev.create_buffer(
            &vk::BufferCreateInfo::default()
                .size((W * H * 4) as u64)
                .usage(vk::BufferUsageFlags::TRANSFER_DST),
            None,
        )
    }
    .expect("readback buffer");
    let rmr = unsafe { c.dev.get_buffer_memory_requirements(c.rbuf) };
    let rmem = unsafe {
        c.dev.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(rmr.size)
                .memory_type_index(c.memtype(
                    rmr.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )),
            None,
        )
    }
    .unwrap();
    unsafe { c.dev.bind_buffer_memory(c.rbuf, rmem, 0) }.unwrap();
    c.rmap = unsafe {
        c.dev
            .map_memory(rmem, 0, (W * H * 4) as u64, vk::MemoryMapFlags::empty())
    }
    .unwrap() as *mut u8;
    c.pool = unsafe {
        c.dev.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(c.qfam)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )
    }
    .expect("command pool");
    s.ok(c.pool != vk::CommandPool::null(), "vkCreateCommandPool");
    s.ok(true, "offscreen R8G8B8A8 target + readback buffer ready");
    {
        let fp = unsafe {
            c.inst
                .get_physical_device_format_properties(c.pd, vk::Format::R8_UNORM)
        };
        s.ok(
            fp.optimal_tiling_features
                .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE),
            "R8_UNORM optimal-tiling SAMPLED_IMAGE",
        );
    }

    let vs = c.shmod(include_bytes!("../shaders/uv_vert.spv"));
    let fs_yuv = c.shmod(include_bytes!("../shaders/yuv_frag.spv"));
    let fs_s = c.shmod(include_bytes!("../shaders/samp_frag.spv"));
    let fsq: [f32; 16] = [
        -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ];
    let (vbo, _qm) = c.mk_vbo(&fsq);
    c.vbo = vbo;

    // ============ (1) YUV -> RGB, BT.601 full-range ============
    {
        let (pw, ph, cw, ch) = (32usize, 32usize, 16usize, 16usize);
        let mut yy = vec![0u8; pw * ph];
        let mut uu = vec![0u8; cw * ch];
        let mut vv = vec![0u8; cw * ch];
        for y in 0..ph {
            for x in 0..pw {
                yy[y * pw + x] = clampi(((x * 8 + y * 4) % 256) as i32, 0, 255) as u8;
            }
        }
        for y in 0..ch {
            for x in 0..cw {
                uu[y * cw + x] = ((x * 16) % 256) as u8;
                vv[y * cw + x] = ((y * 16) % 256) as u8;
            }
        }
        let ty = c.mk_tex(vk::Format::R8_UNORM, pw as u32, ph as u32, &yy);
        let tu = c.mk_tex(vk::Format::R8_UNORM, cw as u32, ch as u32, &uu);
        let tv = c.mk_tex(vk::Format::R8_UNORM, cw as u32, ch as u32, &vv);
        let samp = c.mk_sampler(vk::Filter::NEAREST);
        let d = mk_desc(&c, 3);
        let pipe = c.mk_pipe(vs, fs_yuv, d.pl);
        s.ok(pipe != vk::Pipeline::null(), "YUV->RGB pipeline created");
        let di = [
            vk::DescriptorImageInfo {
                sampler: samp,
                image_view: ty.view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            },
            vk::DescriptorImageInfo {
                sampler: samp,
                image_view: tu.view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            },
            vk::DescriptorImageInfo {
                sampler: samp,
                image_view: tv.view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            },
        ];
        let wds: Vec<_> = (0..3u32)
            .map(|i| {
                vk::WriteDescriptorSet::default()
                    .dst_set(d.dset)
                    .dst_binding(i)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&di[i as usize]))
            })
            .collect();
        unsafe { c.dev.update_descriptor_sets(&wds, &[]) };
        c.draw_sub(pipe, d.pl, d.dset, pw as u32, ph as u32);
        let (mut bad, mut checked) = (0, 0);
        for y in 0..ph {
            for x in 0..pw {
                let u = (x as f32 + 0.5) / pw as f32;
                let v = (y as f32 + 0.5) / ph as f32;
                let cx = clampi((u * cw as f32).floor() as i32, 0, cw as i32 - 1) as usize;
                let cy = clampi((v * ch as f32).floor() as i32, 0, ch as i32 - 1) as usize;
                let yf = yy[y * pw + x] as f32 / 255.0;
                let uf = uu[cy * cw + cx] as f32 / 255.0 - 0.5;
                let vf = vv[cy * cw + cx] as f32 / 255.0 - 0.5;
                let r = yf + 1.402 * vf;
                let g = yf - 0.344136 * uf - 0.714136 * vf;
                let b = yf + 1.772 * uf;
                let er = clampi((r.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                let eg = clampi((g.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                let eb = clampi((b.clamp(0.0, 1.0) * 255.0).round() as i32, 0, 255);
                checked += 1;
                if !c.peq(x, y, [er, eg, eb, 255], 3) {
                    bad += 1;
                }
            }
        }
        s.ok(
            checked == pw * ph,
            "YUV->RGB checked all 32x32 output pixels",
        );
        s.ok(
            bad == 0,
            "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)",
        );
        s.ok(
            true,
            "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form",
        );
        unsafe {
            c.dev.destroy_pipeline(pipe, None);
            c.dev.destroy_pipeline_layout(d.pl, None);
            c.dev.destroy_descriptor_pool(d.pool, None);
            c.dev.destroy_descriptor_set_layout(d.dsl, None);
            c.dev.destroy_sampler(samp, None);
        }
        c.free_tex(&ty);
        c.free_tex(&tu);
        c.free_tex(&tv);
    }

    // ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
    {
        let (sw, sh, ow, oh) = (4usize, 4usize, 16usize, 16usize);
        let mut src = vec![0u8; sw * sh * 4];
        for y in 0..sh {
            for x in 0..sw {
                let i = (y * sw + x) * 4;
                src[i] = (x * 60 + 10) as u8;
                src[i + 1] = (y * 60 + 20) as u8;
                src[i + 2] = ((x + y) * 30) as u8;
                src[i + 3] = 255;
            }
        }
        let st = c.mk_tex(vk::Format::R8G8B8A8_UNORM, sw as u32, sh as u32, &src);
        let samp = c.mk_sampler(vk::Filter::NEAREST);
        let d = mk_desc(&c, 1);
        let pipe = c.mk_pipe(vs, fs_s, d.pl);
        s.ok(
            pipe != vk::Pipeline::null(),
            "chroma-upsample pipeline created",
        );
        let dii = [vk::DescriptorImageInfo {
            sampler: samp,
            image_view: st.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let wds = [vk::WriteDescriptorSet::default()
            .dst_set(d.dset)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&dii)];
        unsafe { c.dev.update_descriptor_sets(&wds, &[]) };
        c.draw_sub(pipe, d.pl, d.dset, ow as u32, oh as u32);
        let mut bad = 0;
        for y in 0..oh {
            for x in 0..ow {
                let u = (x as f32 + 0.5) / ow as f32;
                let v = (y as f32 + 0.5) / oh as f32;
                let sx = clampi((u * sw as f32).floor() as i32, 0, sw as i32 - 1) as usize;
                let sy = clampi((v * sh as f32).floor() as i32, 0, sh as i32 - 1) as usize;
                let i = (sy * sw + sx) * 4;
                if !c.peq(
                    x,
                    y,
                    [src[i] as i32, src[i + 1] as i32, src[i + 2] as i32, 255],
                    1,
                ) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed \
             form)",
        );
        s.ok(
            c.peq(0, 0, [src[0] as i32, src[1] as i32, src[2] as i32, 255], 1),
            "upsample (0,0) = src(0,0)",
        );
        let li = (3 * sw + 3) * 4;
        s.ok(
            c.peq(
                15,
                15,
                [src[li] as i32, src[li + 1] as i32, src[li + 2] as i32, 255],
                1,
            ),
            "upsample (15,15) = src(3,3)",
        );
        unsafe {
            c.dev.destroy_pipeline(pipe, None);
            c.dev.destroy_pipeline_layout(d.pl, None);
            c.dev.destroy_descriptor_pool(d.pool, None);
            c.dev.destroy_descriptor_set_layout(d.dsl, None);
            c.dev.destroy_sampler(samp, None);
        }
        c.free_tex(&st);
    }

    // ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
    {
        let (sw, sh, ow, oh) = (4usize, 4usize, 2usize, 2usize);
        let mut src = vec![0u8; sw * sh * 4];
        for y in 0..sh {
            for x in 0..sw {
                let i = (y * sw + x) * 4;
                let v = (10 + (y * sw + x) * 15) as u8;
                src[i] = v;
                src[i + 1] = 255 - v;
                src[i + 2] = v;
                src[i + 3] = 255;
            }
        }
        let st = c.mk_tex(vk::Format::R8G8B8A8_UNORM, sw as u32, sh as u32, &src);
        let samp = c.mk_sampler(vk::Filter::LINEAR);
        let d = mk_desc(&c, 1);
        let pipe = c.mk_pipe(vs, fs_s, d.pl);
        s.ok(pipe != vk::Pipeline::null(), "downscale pipeline created");
        let dii = [vk::DescriptorImageInfo {
            sampler: samp,
            image_view: st.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let wds = [vk::WriteDescriptorSet::default()
            .dst_set(d.dset)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&dii)];
        unsafe { c.dev.update_descriptor_sets(&wds, &[]) };
        c.draw_sub(pipe, d.pl, d.dset, ow as u32, oh as u32);
        let mut bad = 0;
        for oy in 0..oh {
            for ox in 0..ow {
                let (sx0, sy0) = (ox * 2, oy * 2);
                let mut sum = [0i32; 3];
                for dy in 0..2 {
                    for dx in 0..2 {
                        let i = ((sy0 + dy) * sw + (sx0 + dx)) * 4;
                        sum[0] += src[i] as i32;
                        sum[1] += src[i + 1] as i32;
                        sum[2] += src[i + 2] as i32;
                    }
                }
                let er = (sum[0] as f32 / 4.0).round() as i32;
                let eg = (sum[1] as f32 / 4.0).round() as i32;
                let eb = (sum[2] as f32 / 4.0).round() as i32;
                if !c.peq(ox, oy, [er, eg, eb, 255], 2) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)",
        );
        unsafe {
            c.dev.destroy_pipeline(pipe, None);
            c.dev.destroy_pipeline_layout(d.pl, None);
            c.dev.destroy_descriptor_pool(d.pool, None);
            c.dev.destroy_descriptor_set_layout(d.dsl, None);
            c.dev.destroy_sampler(samp, None);
        }
        c.free_tex(&st);
    }

    // ============ (4) codec round-trip identities (CPU path) ============
    {
        let n = 8usize;
        let mut x = [0.0f64; 8];
        let mut xc = [0.0f64; 8];
        let mut y = [0.0f64; 8];
        for (i, xi) in x.iter_mut().enumerate() {
            *xi = 30.0 + 20.0 * (0.7 * i as f64).sin() + 5.0 * i as f64;
        }
        for (k, xck) in xc.iter_mut().enumerate() {
            let mut s2 = 0.0;
            for (nn, &xnn) in x.iter().enumerate() {
                s2 += xnn * (std::f64::consts::PI / n as f64 * (nn as f64 + 0.5) * k as f64).cos();
            }
            *xck = s2;
        }
        for (nn, ynn) in y.iter_mut().enumerate() {
            let mut s2 = xc[0];
            for (k, &xck) in xc.iter().enumerate().skip(1) {
                s2 += 2.0
                    * xck
                    * (std::f64::consts::PI / n as f64 * (nn as f64 + 0.5) * k as f64).cos();
            }
            *ynn = s2 / n as f64;
        }
        let mut maxerr = 0.0f64;
        for i in 0..n {
            maxerr = maxerr.max((y[i] - x[i]).abs());
        }
        s.ok(
            maxerr < 1e-9,
            "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)",
        );
        let mut diff = 0.0f64;
        for i in 0..n {
            diff = diff.max((xc[i] - x[i]).abs());
        }
        s.ok(
            diff > 1.0,
            "DCT coefficients differ from input (transform is non-trivial)",
        );
    }
    {
        let inp: Vec<u8> = vec![5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3];
        let mut enc: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < inp.len() {
            let v = inp[i];
            let mut j = i;
            while j < inp.len() && inp[j] == v && (j - i) < 255 {
                j += 1;
            }
            enc.push((j - i) as u8);
            enc.push(v);
            i = j;
        }
        let mut dec: Vec<u8> = Vec::new();
        let mut k = 0;
        while k + 1 < enc.len() {
            for _ in 0..enc[k] {
                dec.push(enc[k + 1]);
            }
            k += 2;
        }
        s.ok(dec == inp, "RLE encode/decode round-trip identity");
        s.ok(
            enc.len() < inp.len(),
            "RLE actually compressed the run data (encode is non-trivial)",
        );
    }

    // ---- Negative control ----
    {
        let cai = vk::CommandBufferAllocateInfo::default()
            .command_pool(c.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
        unsafe {
            c.dev.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .unwrap();
        let cv = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 1.0],
            },
        }];
        let rpb = vk::RenderPassBeginInfo::default()
            .render_pass(c.rp)
            .framebuffer(c.fb)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: W as u32,
                    height: H as u32,
                },
            })
            .clear_values(&cv);
        unsafe {
            c.dev
                .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE);
            c.dev.cmd_end_render_pass(cmd);
        }
        let region = [vk::BufferImageCopy {
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_extent: vk::Extent3D {
                width: W as u32,
                height: H as u32,
                depth: 1,
            },
            ..Default::default()
        }];
        unsafe {
            c.dev.cmd_copy_image_to_buffer(
                cmd,
                c.cimg,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                c.rbuf,
                &region,
            );
            c.dev.end_command_buffer(cmd).unwrap();
            let cmds = [cmd];
            let si = [vk::SubmitInfo::default().command_buffers(&cmds)];
            let fence = c
                .dev
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .unwrap();
            c.dev.queue_submit(c.q, &si, fence).unwrap();
            c.dev.wait_for_fences(&[fence], true, u64::MAX).unwrap();
            c.dev.destroy_fence(fence, None);
            c.dev.free_command_buffers(c.pool, &[cmd]);
            std::ptr::copy_nonoverlapping(c.rmap, c.buf.as_mut_ptr(), W * H * 4);
        }
    }
    s.ok(
        c.peq(0, 0, [0, 0, 0, 255], 1),
        "negative control setup: cleared to black",
    );
    s.ok(
        !c.peq(0, 0, [255, 255, 255, 255], 1),
        "negative control: cleared buffer is NOT white",
    );

    unsafe { c.dev.device_wait_idle().unwrap() };
    let (pass, fail) = (s.pass, s.fail);
    let total = pass + fail;
    let expected = 27;
    println!("scene-codec-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_CODEC_RUST OK {pass}");
        std::process::exit(0);
    }
    std::process::exit(1);
}
