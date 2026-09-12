// vulkan_render_rust_full_api - Vulkan RENDER carpet on Mesa lavapipe (software Vulkan on the CPU, no
// GPU/window/surface/swapchain), Rust `ash` binding of the same offscreen render pipeline as the C cell.
// `ash::Entry::load()` dynamically loads libvulkan.so.1; the cell builds an offscreen render pass into an
// R8G8B8A8_UNORM color image, draws through a real graphics pipeline (SPIR-V vertex+fragment shaders loaded
// via ash::util::read_spv), copies the image to a host-visible buffer with cmd_copy_image_to_buffer, maps
// it, and checks every pixel against a closed-form reference for: render-pass clear, a solid quad
// (push-constant color), a per-vertex axis-aligned linear gradient (a triangle-strip quad interpolates
// per-triangle, so only an axis-aligned gradient matches a full-quad closed form), a gl_FragCoord
// checkerboard, a dynamic scissor, alpha blending (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all channels incl
// alpha, so a=191), a sub-rectangle readback. Exhaustive per-API coverage builds a pipeline per state:
// primitive topologies (TRIANGLE_LIST/FAN/LINE_LIST/LINE_STRIP/POINT_LIST), a blend factor+op matrix
// (ONE/ZERO, ONE/ONE, ZERO/ONE, DST_COLOR; ADD/MAX/REVERSE_SUBTRACT), the full depth-func matrix (all 8
// VkCompareOp against a D32_SFLOAT attachment; Vulkan NDC z in [0,1] so a z=0.5 quad vs clear-depth 0.75
// draws for {ALWAYS,LESS,LEQUAL,NOTEQUAL} only), face culling + winding (NONE/FRONT_AND_BACK/BACK x
// CCW/CW), a color write mask, format+device property queries, and a 2x2 texture upload + NEAREST sampling
// through a combined image sampler + descriptor set, closing with a negative control. Prints
// "VULKAN_RENDER_RUST_FULL_API OK <n>" only when every assertion passes and count == EXPECTED (68).

use std::io::Cursor;

use ash::{Device, Entry, Instance, vk};

const W: u32 = 64;
const H: u32 = 64;

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

// Whole render context; every handle mirrors a `static` in the C cell.
struct Ctx {
    #[allow(dead_code)] // must outlive all Vulkan handles: keeps libvulkan.so.1 loaded
    entry: Entry,
    inst: Instance,
    pd: vk::PhysicalDevice,
    dev: Device,
    q: vk::Queue,
    qfam: u32,
    pool: vk::CommandPool,
    cimg: vk::Image,
    cview: vk::ImageView,
    rp: vk::RenderPass,
    fb: vk::Framebuffer,
    rp_d: vk::RenderPass,
    fb_d: vk::Framebuffer,
    rbuf: vk::Buffer,
    rmap: *mut u8,
    px: Vec<u8>,
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
        let ci = vk::ShaderModuleCreateInfo::default().code(&code);
        unsafe { self.dev.create_shader_module(&ci, None) }.expect("shader module")
    }

    fn mk_layout(&self, push_const: bool) -> vk::PipelineLayout {
        let pcr = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: 16,
        }];
        let mut li = vk::PipelineLayoutCreateInfo::default();
        if push_const {
            li = li.push_constant_ranges(&pcr);
        }
        unsafe { self.dev.create_pipeline_layout(&li, None) }.expect("pipeline layout")
    }

    fn mk_vbo(&self, data: &[f32]) -> (vk::Buffer, vk::DeviceMemory) {
        let sz = std::mem::size_of_val(data) as vk::DeviceSize;
        let bi = vk::BufferCreateInfo::default()
            .size(sz)
            .usage(vk::BufferUsageFlags::VERTEX_BUFFER);
        let b = unsafe { self.dev.create_buffer(&bi, None) }.expect("buffer");
        let mr = unsafe { self.dev.get_buffer_memory_requirements(b) };
        let ai = vk::MemoryAllocateInfo::default()
            .allocation_size(mr.size)
            .memory_type_index(self.memtype(
                mr.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ));
        let mem = unsafe { self.dev.allocate_memory(&ai, None) }.expect("vbo memory");
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
}

// Simple pipeline builder matching the C `mkPipe` (triangle-strip, dynamic scissor, SRC_ALPHA blend).
#[allow(clippy::too_many_arguments)]
fn mk_pipe(
    c: &Ctx,
    vs: vk::ShaderModule,
    fs: vk::ShaderModule,
    pl: vk::PipelineLayout,
    with_color_attr: bool,
    blend: bool,
) -> vk::Pipeline {
    mk_pipe2(
        c,
        vs,
        fs,
        pl,
        if with_color_attr { 1 } else { 0 },
        vk::PrimitiveTopology::TRIANGLE_STRIP,
        blend,
        vk::BlendFactor::SRC_ALPHA,
        vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        vk::BlendOp::ADD,
        vk::BlendFactor::SRC_ALPHA,
        vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        vk::BlendOp::ADD,
        vk::CullModeFlags::NONE,
        vk::FrontFace::COUNTER_CLOCKWISE,
        false,
        vk::CompareOp::ALWAYS,
        c.rp,
        vk::ColorComponentFlags::RGBA,
    )
}

// Rich pipeline builder for exhaustive per-API coverage: vertex layout (0=pos2, 1=pos2+color, 2=pos3,
// 3=pos2+uv), topology, full blend factor+op config, cull mode + winding, depth test + compare op, the
// render pass to use, and a color write mask. Mirrors the C `mkPipe2`.
#[allow(clippy::too_many_arguments)]
fn mk_pipe2(
    c: &Ctx,
    vs: vk::ShaderModule,
    fs: vk::ShaderModule,
    pl: vk::PipelineLayout,
    vlayout: u32,
    topo: vk::PrimitiveTopology,
    blend: bool,
    s_c: vk::BlendFactor,
    d_c: vk::BlendFactor,
    o_c: vk::BlendOp,
    s_a: vk::BlendFactor,
    d_a: vk::BlendFactor,
    o_a: vk::BlendOp,
    cull: vk::CullModeFlags,
    front: vk::FrontFace,
    depth_test: bool,
    depth_op: vk::CompareOp,
    rp_use: vk::RenderPass,
    cwmask: vk::ColorComponentFlags,
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
    let stride = match vlayout {
        0 => 8,
        1 => 24,
        2 => 12,
        _ => 16,
    };
    let bind = [vk::VertexInputBindingDescription {
        binding: 0,
        stride,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attr0 = if vlayout == 2 {
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R32G32B32_SFLOAT,
            offset: 0,
        }
    } else {
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R32G32_SFLOAT,
            offset: 0,
        }
    };
    let mut attr = vec![attr0];
    if vlayout == 1 {
        attr.push(vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R32G32B32A32_SFLOAT,
            offset: 8,
        });
    } else if vlayout == 3 {
        attr.push(vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R32G32_SFLOAT,
            offset: 8,
        });
    }
    let vi = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bind)
        .vertex_attribute_descriptions(&attr);
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(topo);
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
            width: W,
            height: H,
        },
    }];
    let vps = vk::PipelineViewportStateCreateInfo::default()
        .viewports(&vp)
        .scissors(&sc);
    let dyn_states = [vk::DynamicState::SCISSOR];
    let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
    let rs = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(cull)
        .front_face(front)
        .line_width(1.0);
    let ms = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let dss = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(depth_test)
        .depth_write_enable(depth_test)
        .depth_compare_op(depth_op)
        .min_depth_bounds(0.0)
        .max_depth_bounds(1.0);
    let cba = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(blend)
        .src_color_blend_factor(s_c)
        .dst_color_blend_factor(d_c)
        .color_blend_op(o_c)
        .src_alpha_blend_factor(s_a)
        .dst_alpha_blend_factor(d_a)
        .alpha_blend_op(o_a)
        .color_write_mask(cwmask)];
    let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
    let gp = [vk::GraphicsPipelineCreateInfo::default()
        .stages(&st)
        .vertex_input_state(&vi)
        .input_assembly_state(&ia)
        .viewport_state(&vps)
        .rasterization_state(&rs)
        .multisample_state(&ms)
        .color_blend_state(&cb)
        .depth_stencil_state(&dss)
        .dynamic_state(&ds)
        .layout(pl)
        .render_pass(rp_use)
        .subpass(0)];
    unsafe {
        c.dev
            .create_graphics_pipelines(vk::PipelineCache::null(), &gp, None)
    }
    .expect("graphics pipeline")[0]
}

// The draw inputs shared by the color-only and depth-enabled frame paths.
struct Draw<'a> {
    pipe: vk::Pipeline,
    pl: vk::PipelineLayout,
    push_color: Option<&'a [f32; 4]>,
    vbo: vk::Buffer,
    verts: u32,
}

// One-shot record+submit+wait: clear color, optional draw, copy color image to readback buffer, capture px.
fn frame(c: &mut Ctx, clear: [f32; 4], d: Draw, scissor: vk::Rect2D) {
    let Draw {
        pipe,
        pl,
        push_color,
        vbo,
        verts,
    } = d;
    let cai = vk::CommandBufferAllocateInfo::default()
        .command_pool(c.pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
    let bi =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    unsafe { c.dev.begin_command_buffer(cmd, &bi) }.unwrap();
    let cv = [vk::ClearValue {
        color: vk::ClearColorValue { float32: clear },
    }];
    let rpb = vk::RenderPassBeginInfo::default()
        .render_pass(c.rp)
        .framebuffer(c.fb)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: W,
                height: H,
            },
        })
        .clear_values(&cv);
    unsafe {
        c.dev
            .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE);
        if pipe != vk::Pipeline::null() {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
            c.dev.cmd_set_scissor(cmd, 0, &[scissor]);
            if let Some(col) = push_color {
                let bytes = std::slice::from_raw_parts(col.as_ptr() as *const u8, 16);
                c.dev
                    .cmd_push_constants(cmd, pl, vk::ShaderStageFlags::FRAGMENT, 0, bytes);
            }
            if vbo != vk::Buffer::null() {
                c.dev.cmd_bind_vertex_buffers(cmd, 0, &[vbo], &[0]);
            }
            c.dev.cmd_draw(cmd, verts, 1, 0, 0);
        }
        c.dev.cmd_end_render_pass(cmd);
    }
    copy_and_submit(c, cmd);
}

// depth-enabled frame: clears color + depth, uses the depth render pass/framebuffer, draws vec3 quad.
fn frame_d(c: &mut Ctx, clear: [f32; 4], depth_clear: f32, d: Draw) {
    let Draw {
        pipe,
        pl,
        push_color,
        vbo,
        verts,
    } = d;
    let cai = vk::CommandBufferAllocateInfo::default()
        .command_pool(c.pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
    let bi =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    unsafe { c.dev.begin_command_buffer(cmd, &bi) }.unwrap();
    let cv = [
        vk::ClearValue {
            color: vk::ClearColorValue { float32: clear },
        },
        vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: depth_clear,
                stencil: 0,
            },
        },
    ];
    let rpb = vk::RenderPassBeginInfo::default()
        .render_pass(c.rp_d)
        .framebuffer(c.fb_d)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: W,
                height: H,
            },
        })
        .clear_values(&cv);
    let scissor = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D {
            width: W,
            height: H,
        },
    };
    unsafe {
        c.dev
            .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE);
        c.dev
            .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
        c.dev.cmd_set_scissor(cmd, 0, &[scissor]);
        if let Some(col) = push_color {
            let bytes = std::slice::from_raw_parts(col.as_ptr() as *const u8, 16);
            c.dev
                .cmd_push_constants(cmd, pl, vk::ShaderStageFlags::FRAGMENT, 0, bytes);
        }
        if vbo != vk::Buffer::null() {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[vbo], &[0]);
        }
        c.dev.cmd_draw(cmd, verts, 1, 0, 0);
        c.dev.cmd_end_render_pass(cmd);
    }
    copy_and_submit(c, cmd);
}

// Shared tail of frame/frame_d: copy the color image into the readback buffer, submit, wait, capture px.
fn copy_and_submit(c: &mut Ctx, cmd: vk::CommandBuffer) {
    let region = [vk::BufferImageCopy {
        image_subresource: vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        },
        image_extent: vk::Extent3D {
            width: W,
            height: H,
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
        std::ptr::copy_nonoverlapping(c.rmap, c.px.as_mut_ptr(), (W * H * 4) as usize);
    }
}

fn px(c: &Ctx, x: u32, y: u32, ch: u32) -> i32 {
    c.px[((y * W + x) * 4 + ch) as usize] as i32
}
fn peq(c: &Ctx, x: u32, y: u32, rgba: [i32; 4], tol: i32) -> bool {
    (px(c, x, y, 0) - rgba[0]).abs() <= tol
        && (px(c, x, y, 1) - rgba[1]).abs() <= tol
        && (px(c, x, y, 2) - rgba[2]).abs() <= tol
        && (px(c, x, y, 3) - rgba[3]).abs() <= tol
}
fn all_eq(c: &Ctx, r: i32, g: i32, b: i32, a: i32, tol: i32) -> bool {
    (0..H).all(|y| (0..W).all(|x| peq(c, x, y, [r, g, b, a], tol)))
}

fn main() {
    let mut s = Score { pass: 0, fail: 0 };

    let entry = unsafe { Entry::load() }.expect("failed to load libvulkan.so.1");
    let app = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
    let ici = vk::InstanceCreateInfo::default().application_info(&app);
    let inst = unsafe { entry.create_instance(&ici, None) }.expect("vkCreateInstance");
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
    let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qci);
    let dev = unsafe { inst.create_device(pd, &dci, None) }.expect("vkCreateDevice");
    s.ok(true, "vkCreateDevice");
    let q = unsafe { dev.get_device_queue(qfam, 0) };

    // Build the context skeleton; image/renderpass/buffer handles filled in below.
    let mut c = Ctx {
        entry,
        inst,
        pd,
        dev,
        q,
        qfam,
        pool: vk::CommandPool::null(),
        cimg: vk::Image::null(),
        cview: vk::ImageView::null(),
        rp: vk::RenderPass::null(),
        fb: vk::Framebuffer::null(),
        rp_d: vk::RenderPass::null(),
        fb_d: vk::Framebuffer::null(),
        rbuf: vk::Buffer::null(),
        rmap: std::ptr::null_mut(),
        px: vec![0u8; (W * H * 4) as usize],
    };

    // color image (R8G8B8A8_UNORM) + view
    let ii = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: W,
            height: H,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    c.cimg = unsafe { c.dev.create_image(&ii, None) }.expect("color image");
    s.ok(true, "vkCreateImage color");
    let imr = unsafe { c.dev.get_image_memory_requirements(c.cimg) };
    let iai = vk::MemoryAllocateInfo::default()
        .allocation_size(imr.size)
        .memory_type_index(c.memtype(imr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL));
    let cmem = unsafe { c.dev.allocate_memory(&iai, None) }.unwrap();
    unsafe { c.dev.bind_image_memory(c.cimg, cmem, 0) }.unwrap();
    let vi = vk::ImageViewCreateInfo::default()
        .image(c.cimg)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    c.cview = unsafe { c.dev.create_image_view(&vi, None) }.expect("color view");
    s.ok(true, "vkCreateImageView");

    // color-only render pass + framebuffer
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
    let rpi = vk::RenderPassCreateInfo::default()
        .attachments(&att)
        .subpasses(&sp);
    c.rp = unsafe { c.dev.create_render_pass(&rpi, None) }.expect("render pass");
    s.ok(true, "vkCreateRenderPass");
    let fbv = [c.cview];
    let fbi = vk::FramebufferCreateInfo::default()
        .render_pass(c.rp)
        .attachments(&fbv)
        .width(W)
        .height(H)
        .layers(1);
    c.fb = unsafe { c.dev.create_framebuffer(&fbi, None) }.expect("framebuffer");
    s.ok(true, "vkCreateFramebuffer");

    // depth resources: D32_SFLOAT image + a color+depth render pass sharing cimg
    let dii = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::D32_SFLOAT)
        .extent(vk::Extent3D {
            width: W,
            height: H,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let dimg = unsafe { c.dev.create_image(&dii, None) }.expect("depth image");
    s.ok(true, "vkCreateImage depth");
    let dmr = unsafe { c.dev.get_image_memory_requirements(dimg) };
    let daii = vk::MemoryAllocateInfo::default()
        .allocation_size(dmr.size)
        .memory_type_index(c.memtype(dmr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL));
    let dmem = unsafe { c.dev.allocate_memory(&daii, None) }.unwrap();
    unsafe { c.dev.bind_image_memory(dimg, dmem, 0) }.unwrap();
    let dvi = vk::ImageViewCreateInfo::default()
        .image(dimg)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::D32_SFLOAT)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::DEPTH,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    let dview = unsafe { c.dev.create_image_view(&dvi, None) }.expect("depth view");
    s.ok(true, "vkCreateImageView depth");
    let datt = [
        vk::AttachmentDescription::default()
            .format(vk::Format::R8G8B8A8_UNORM)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL),
        vk::AttachmentDescription::default()
            .format(vk::Format::D32_SFLOAT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
    ];
    let dcref = [vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    }];
    let ddref = vk::AttachmentReference {
        attachment: 1,
        layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    };
    let dsp = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&dcref)
        .depth_stencil_attachment(&ddref)];
    let drpi = vk::RenderPassCreateInfo::default()
        .attachments(&datt)
        .subpasses(&dsp);
    c.rp_d = unsafe { c.dev.create_render_pass(&drpi, None) }.expect("depth render pass");
    s.ok(true, "vkCreateRenderPass depth");
    let dfbv = [c.cview, dview];
    let dfbi = vk::FramebufferCreateInfo::default()
        .render_pass(c.rp_d)
        .attachments(&dfbv)
        .width(W)
        .height(H)
        .layers(1);
    c.fb_d = unsafe { c.dev.create_framebuffer(&dfbi, None) }.expect("depth framebuffer");
    s.ok(true, "vkCreateFramebuffer depth");

    // host-visible readback buffer
    let rbi = vk::BufferCreateInfo::default()
        .size((W * H * 4) as vk::DeviceSize)
        .usage(vk::BufferUsageFlags::TRANSFER_DST);
    c.rbuf = unsafe { c.dev.create_buffer(&rbi, None) }.expect("readback buffer");
    let rmr = unsafe { c.dev.get_buffer_memory_requirements(c.rbuf) };
    let rai = vk::MemoryAllocateInfo::default()
        .allocation_size(rmr.size)
        .memory_type_index(c.memtype(
            rmr.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ));
    let rmem = unsafe { c.dev.allocate_memory(&rai, None) }.unwrap();
    unsafe { c.dev.bind_buffer_memory(c.rbuf, rmem, 0) }.unwrap();
    c.rmap = unsafe {
        c.dev.map_memory(
            rmem,
            0,
            (W * H * 4) as vk::DeviceSize,
            vk::MemoryMapFlags::empty(),
        )
    }
    .unwrap() as *mut u8;

    let pci = vk::CommandPoolCreateInfo::default()
        .queue_family_index(c.qfam)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
    c.pool = unsafe { c.dev.create_command_pool(&pci, None) }.expect("command pool");
    s.ok(true, "vkCreateCommandPool");

    // shaders
    let vs_s = c.shmod(include_bytes!("../shaders/solid_vert.spv"));
    let fs_s = c.shmod(include_bytes!("../shaders/solid_frag.spv"));
    let vs_g = c.shmod(include_bytes!("../shaders/grad_vert.spv"));
    let fs_g = c.shmod(include_bytes!("../shaders/grad_frag.spv"));
    let fs_c = c.shmod(include_bytes!("../shaders/check_frag.spv"));
    let vs_pt = c.shmod(include_bytes!("../shaders/point_vert.spv"));
    let vs_p3 = c.shmod(include_bytes!("../shaders/pos3_vert.spv"));
    let vs_tx = c.shmod(include_bytes!("../shaders/tex_vert.spv"));
    let fs_tx = c.shmod(include_bytes!("../shaders/tex_frag.spv"));

    let pl_push = c.mk_layout(true);
    let pl_none = c.mk_layout(false);
    let pipe_solid = mk_pipe(&c, vs_s, fs_s, pl_push, false, false);
    let pipe_blend = mk_pipe(&c, vs_s, fs_s, pl_push, false, true);
    let pipe_grad = mk_pipe(&c, vs_g, fs_g, pl_none, true, false);
    let pipe_check = mk_pipe(&c, vs_s, fs_c, pl_none, false, false);
    s.ok(
        [pipe_solid, pipe_grad, pipe_check, pipe_blend]
            .iter()
            .all(|p| *p != vk::Pipeline::null()),
        "graphics pipelines created",
    );

    let quad: [f32; 8] = [-1., -1., 1., -1., -1., 1., 1., 1.];
    let gquad: [f32; 24] = [
        -1., -1., 1., 0., 0., 1., 1., -1., 0., 0., 1., 1., -1., 1., 1., 0., 0., 1., 1., 1., 0., 0.,
        1., 1.,
    ];
    let (vbo, qm) = c.mk_vbo(&quad);
    let (gvbo, gm) = c.mk_vbo(&gquad);
    let full = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D {
            width: W,
            height: H,
        },
    };

    // Test 1: render-pass clear
    frame(
        &mut c,
        [0.0, 0.25, 0.5, 1.0],
        Draw {
            pipe: vk::Pipeline::null(),
            pl: pl_none,
            push_color: None,
            vbo: vk::Buffer::null(),
            verts: 0,
        },
        full,
    );
    s.ok(
        all_eq(&c, 0, 64, 128, 255, 2),
        "renderpass clear (0,0.25,0.5,1) all pixels (0,64,128,255)",
    );
    s.ok(peq(&c, 0, 0, [0, 64, 128, 255], 2), "clear pixel (0,0)");

    // Test 2: solid red push-constant quad
    frame(
        &mut c,
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: pipe_solid,
            pl: pl_push,
            push_color: Some(&[1., 0., 0., 1.]),
            vbo,
            verts: 4,
        },
        full,
    );
    s.ok(
        all_eq(&c, 255, 0, 0, 255, 1),
        "solid red quad fills every pixel",
    );

    // Test 3: axis-aligned gradient
    frame(
        &mut c,
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: pipe_grad,
            pl: pl_none,
            push_color: None,
            vbo: gvbo,
            verts: 4,
        },
        full,
    );
    {
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let u = (x as f32 + 0.5) / W as f32;
                let r = ((1.0 - u) * 255.0).round() as i32;
                let b = (u * 255.0).round() as i32;
                if !peq(&c, x, y, [r, 0, b, 255], 4) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "gradient matches horizontal-linear closed-form for all pixels",
        );
        s.ok(
            peq(&c, 0, 0, [255, 0, 0, 255], 8),
            "gradient left edge ~ red",
        );
        s.ok(
            peq(&c, W - 1, H - 1, [0, 0, 255, 255], 8),
            "gradient right edge ~ blue",
        );
        s.ok(
            peq(&c, W / 2, H / 2, [128, 0, 128, 255], 4),
            "gradient center ~ (128,0,128)",
        );
    }

    // Test 4: gl_FragCoord checkerboard
    frame(
        &mut c,
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: pipe_check,
            pl: pl_none,
            push_color: None,
            vbo,
            verts: 4,
        },
        full,
    );
    {
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let e = (((x >> 3) + (y >> 3)) & 1) == 0;
                let w = if e { 255 } else { 0 };
                if !peq(&c, x, y, [w, w, w, 255], 1) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "checkerboard matches (x/8+y/8) parity for all pixels",
        );
        s.ok(
            peq(&c, 0, 0, [255, 255, 255, 255], 1),
            "checker cell (0,0) white",
        );
        s.ok(peq(&c, 8, 0, [0, 0, 0, 255], 1), "checker cell (8,0) black");
    }

    // Test 5: dynamic scissor
    {
        let box_ = vk::Rect2D {
            offset: vk::Offset2D { x: 16, y: 16 },
            extent: vk::Extent2D {
                width: 32,
                height: 32,
            },
        };
        frame(
            &mut c,
            [1.0, 0.0, 0.0, 1.0],
            Draw {
                pipe: pipe_solid,
                pl: pl_push,
                push_color: Some(&[0., 1., 0., 1.]),
                vbo,
                verts: 4,
            },
            box_,
        );
        s.ok(
            peq(&c, 32, 32, [0, 255, 0, 255], 1),
            "scissor: inside box green",
        );
        s.ok(
            peq(&c, 2, 2, [255, 0, 0, 255], 1),
            "scissor: outside box red (clear)",
        );
        s.ok(
            peq(&c, 50, 50, [255, 0, 0, 255], 1),
            "scissor: past box red",
        );
    }

    // Test 6: alpha blend SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all channels
    frame(
        &mut c,
        [1.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: pipe_blend,
            pl: pl_push,
            push_color: Some(&[0., 0., 1., 0.5]),
            vbo,
            verts: 4,
        },
        full,
    );
    s.ok(
        all_eq(&c, 128, 0, 128, 191, 3),
        "alpha blend 0.5*blue over red -> rgb(128,0,128) a191",
    );

    // Test 7: sub-rect readback
    frame(
        &mut c,
        [0.0, 0.0, 0.0, 1.0],
        Draw {
            pipe: pipe_solid,
            pl: pl_push,
            push_color: Some(&[0.2, 0.4, 0.6, 1.0]),
            vbo,
            verts: 4,
        },
        full,
    );
    {
        let mut good = true;
        for y in 10..14 {
            for x in 10..14 {
                if !peq(&c, x, y, [51, 102, 153, 255], 2) {
                    good = false;
                }
            }
        }
        s.ok(good, "sub-rect (10,10,4x4) == (51,102,153,255)");
    }

    // ==================== exhaustive per-API render coverage ====================
    let noblend = (
        false,
        vk::BlendFactor::ONE,
        vk::BlendFactor::ZERO,
        vk::BlendOp::ADD,
        vk::BlendFactor::ONE,
        vk::BlendFactor::ZERO,
        vk::BlendOp::ADD,
    );
    let ccwf = vk::FrontFace::COUNTER_CLOCKWISE;
    let red = [1.0f32, 0., 0., 1.];

    // Test 8: primitive topologies
    {
        let tl: [f32; 12] = [-1., -1., 1., -1., -1., 1., -1., 1., 1., -1., 1., 1.];
        let fan: [f32; 12] = [0., 0., -1., -1., 1., -1., 1., 1., -1., 1., -1., -1.];
        let hln: [f32; 4] = [-1., 0., 1., 0.];
        let pt: [f32; 2] = [0., 0.];
        let (b_tl, m1) = c.mk_vbo(&tl);
        let (b_fan, m2) = c.mk_vbo(&fan);
        let (b_ln, m3) = c.mk_vbo(&hln);
        let (b_pt, m4) = c.mk_vbo(&pt);
        let mkp = |c: &Ctx, topo, vs| {
            mk_pipe2(
                c,
                vs,
                fs_s,
                pl_push,
                0,
                topo,
                noblend.0,
                noblend.1,
                noblend.2,
                noblend.3,
                noblend.4,
                noblend.5,
                noblend.6,
                vk::CullModeFlags::NONE,
                ccwf,
                false,
                vk::CompareOp::ALWAYS,
                c.rp,
                vk::ColorComponentFlags::RGBA,
            )
        };
        let p_tl = mkp(&c, vk::PrimitiveTopology::TRIANGLE_LIST, vs_s);
        let p_fan = mkp(&c, vk::PrimitiveTopology::TRIANGLE_FAN, vs_s);
        let p_ll = mkp(&c, vk::PrimitiveTopology::LINE_LIST, vs_s);
        let p_ls = mkp(&c, vk::PrimitiveTopology::LINE_STRIP, vs_s);
        let p_pt = mkp(&c, vk::PrimitiveTopology::POINT_LIST, vs_pt);
        s.ok(
            [p_tl, p_fan, p_ll, p_ls, p_pt]
                .iter()
                .all(|p| *p != vk::Pipeline::null()),
            "topology pipelines created",
        );
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: p_tl,
                pl: pl_push,
                push_color: Some(&red),
                vbo: b_tl,
                verts: 6,
            },
            full,
        );
        s.ok(all_eq(&c, 255, 0, 0, 255, 1), "TRIANGLE_LIST fills quad");
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: p_fan,
                pl: pl_push,
                push_color: Some(&red),
                vbo: b_fan,
                verts: 6,
            },
            full,
        );
        s.ok(all_eq(&c, 255, 0, 0, 255, 1), "TRIANGLE_FAN fills quad");
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: p_ll,
                pl: pl_push,
                push_color: Some(&red),
                vbo: b_ln,
                verts: 2,
            },
            full,
        );
        {
            let mid = (0..W)
                .filter(|&x| {
                    peq(&c, x, H / 2, [255, 0, 0, 255], 2)
                        || peq(&c, x, H / 2 - 1, [255, 0, 0, 255], 2)
                })
                .count() as u32;
            s.ok(mid >= W - 2, "LINE_LIST draws the middle row");
            s.ok(
                peq(&c, 0, 0, [0, 0, 0, 255], 2),
                "LINE_LIST leaves top row clear",
            );
        }
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: p_ls,
                pl: pl_push,
                push_color: Some(&red),
                vbo: b_ln,
                verts: 2,
            },
            full,
        );
        {
            let mid = (0..W)
                .filter(|&x| {
                    peq(&c, x, H / 2, [255, 0, 0, 255], 2)
                        || peq(&c, x, H / 2 - 1, [255, 0, 0, 255], 2)
                })
                .count() as u32;
            s.ok(mid >= W - 2, "LINE_STRIP draws the middle row");
        }
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: p_pt,
                pl: pl_push,
                push_color: Some(&red),
                vbo: b_pt,
                verts: 1,
            },
            full,
        );
        {
            let mut hit = false;
            for y in (H / 2 - 2)..=(H / 2 + 2) {
                for x in (W / 2 - 2)..=(W / 2 + 2) {
                    if peq(&c, x, y, [255, 0, 0, 255], 2) {
                        hit = true;
                    }
                }
            }
            s.ok(hit, "POINT_LIST draws a pixel at the center");
        }
        unsafe {
            for p in [p_tl, p_fan, p_ll, p_ls, p_pt] {
                c.dev.destroy_pipeline(p, None);
            }
            for (b, m) in [(b_tl, m1), (b_fan, m2), (b_ln, m3), (b_pt, m4)] {
                c.dev.destroy_buffer(b, None);
                c.dev.free_memory(m, None);
            }
        }
    }

    // Test 9: blend factor + op matrix (build pipeline, draw, destroy - one at a time)
    {
        let mkb = |c: &Ctx, s_c, d_c, o_c, s_a, d_a, o_a| {
            mk_pipe2(
                c,
                vs_s,
                fs_s,
                pl_push,
                0,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                true,
                s_c,
                d_c,
                o_c,
                s_a,
                d_a,
                o_a,
                vk::CullModeFlags::NONE,
                ccwf,
                false,
                vk::CompareOp::ALWAYS,
                c.rp,
                vk::ColorComponentFlags::RGBA,
            )
        };
        let pb1 = mkb(
            &c,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ZERO,
            vk::BlendOp::ADD,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ZERO,
            vk::BlendOp::ADD,
        );
        frame(
            &mut c,
            [0.5, 0.5, 0.5, 1.],
            Draw {
                pipe: pb1,
                pl: pl_push,
                push_color: Some(&[0., 0., 1., 1.]),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 0, 0, 255, 255, 2),
            "blend ONE/ZERO: src replaces dst",
        );
        unsafe {
            c.dev.destroy_pipeline(pb1, None);
        }
        let pb2 = mkb(
            &c,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE,
            vk::BlendOp::ADD,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE,
            vk::BlendOp::ADD,
        );
        frame(
            &mut c,
            [0.5, 0., 0., 1.],
            Draw {
                pipe: pb2,
                pl: pl_push,
                push_color: Some(&[0., 0., 0.5, 1.]),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 128, 0, 128, 255, 2),
            "blend ONE/ONE ADD: src+dst = (128,0,128)",
        );
        unsafe {
            c.dev.destroy_pipeline(pb2, None);
        }
        let pb3 = mkb(
            &c,
            vk::BlendFactor::ZERO,
            vk::BlendFactor::ONE,
            vk::BlendOp::ADD,
            vk::BlendFactor::ZERO,
            vk::BlendFactor::ONE,
            vk::BlendOp::ADD,
        );
        frame(
            &mut c,
            [0.2, 0., 0., 1.],
            Draw {
                pipe: pb3,
                pl: pl_push,
                push_color: Some(&[0., 1., 0., 1.]),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 51, 0, 0, 255, 2),
            "blend ZERO/ONE: dst kept (51,0,0)",
        );
        unsafe {
            c.dev.destroy_pipeline(pb3, None);
        }
        let pb4 = mkb(
            &c,
            vk::BlendFactor::DST_COLOR,
            vk::BlendFactor::ZERO,
            vk::BlendOp::ADD,
            vk::BlendFactor::DST_COLOR,
            vk::BlendFactor::ZERO,
            vk::BlendOp::ADD,
        );
        frame(
            &mut c,
            [0.5, 0.5, 0.5, 1.],
            Draw {
                pipe: pb4,
                pl: pl_push,
                push_color: Some(&[0., 0., 1., 1.]),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 0, 0, 128, 255, 2),
            "blend DST_COLOR/ZERO: src*dst modulate (0,0,128)",
        );
        unsafe {
            c.dev.destroy_pipeline(pb4, None);
        }
        let pb5 = mkb(
            &c,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE,
            vk::BlendOp::MAX,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE,
            vk::BlendOp::MAX,
        );
        frame(
            &mut c,
            [0.2, 0.6, 0.2, 1.],
            Draw {
                pipe: pb5,
                pl: pl_push,
                push_color: Some(&[0.6, 0.2, 0.6, 1.]),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 153, 153, 153, 255, 2),
            "blend op MAX: per-channel max",
        );
        unsafe {
            c.dev.destroy_pipeline(pb5, None);
        }
        let pb6 = mkb(
            &c,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE,
            vk::BlendOp::REVERSE_SUBTRACT,
            vk::BlendFactor::ONE,
            vk::BlendFactor::ONE,
            vk::BlendOp::REVERSE_SUBTRACT,
        );
        frame(
            &mut c,
            [1., 0., 0., 1.],
            Draw {
                pipe: pb6,
                pl: pl_push,
                push_color: Some(&[0.25, 0., 0., 1.]),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 191, 0, 0, 0, 3),
            "blend op REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0",
        );
        unsafe {
            c.dev.destroy_pipeline(pb6, None);
        }
    }

    // Test 10: depth-func matrix (Vulkan NDC z in [0,1]: quad z=0.5, clear depth 0.75)
    {
        let dq: [f32; 12] = [-1., -1., 0.5, 1., -1., 0.5, -1., 1., 0.5, 1., 1., 0.5];
        let (vbo3, dm3) = c.mk_vbo(&dq);
        let grn = [0.0f32, 1., 0., 1.];
        let dt: [(vk::CompareOp, bool, &str); 8] = [
            (vk::CompareOp::ALWAYS, true, "ALWAYS"),
            (vk::CompareOp::NEVER, false, "NEVER"),
            (vk::CompareOp::LESS, true, "LESS"),
            (vk::CompareOp::LESS_OR_EQUAL, true, "LEQUAL"),
            (vk::CompareOp::EQUAL, false, "EQUAL"),
            (vk::CompareOp::GREATER, false, "GREATER"),
            (vk::CompareOp::GREATER_OR_EQUAL, false, "GEQUAL"),
            (vk::CompareOp::NOT_EQUAL, true, "NOTEQUAL"),
        ];
        for (op, draws, name) in dt {
            let pdp = mk_pipe2(
                &c,
                vs_p3,
                fs_s,
                pl_push,
                2,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                noblend.0,
                noblend.1,
                noblend.2,
                noblend.3,
                noblend.4,
                noblend.5,
                noblend.6,
                vk::CullModeFlags::NONE,
                ccwf,
                true,
                op,
                c.rp_d,
                vk::ColorComponentFlags::RGBA,
            );
            frame_d(
                &mut c,
                [0., 0., 0., 1.],
                0.75,
                Draw {
                    pipe: pdp,
                    pl: pl_push,
                    push_color: Some(&grn),
                    vbo: vbo3,
                    verts: 4,
                },
            );
            s.ok(peq(&c, W / 2, H / 2, [0, 255, 0, 255], 2) == draws, name);
            unsafe {
                c.dev.destroy_pipeline(pdp, None);
            }
        }
        unsafe {
            c.dev.destroy_buffer(vbo3, None);
            c.dev.free_memory(dm3, None);
        }
    }

    // Test 11: face culling + winding
    {
        let mkc = |c: &Ctx, cull, front| {
            mk_pipe2(
                c,
                vs_s,
                fs_s,
                pl_push,
                0,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                noblend.0,
                noblend.1,
                noblend.2,
                noblend.3,
                noblend.4,
                noblend.5,
                noblend.6,
                cull,
                front,
                false,
                vk::CompareOp::ALWAYS,
                c.rp,
                vk::ColorComponentFlags::RGBA,
            )
        };
        let pcn = mkc(&c, vk::CullModeFlags::NONE, ccwf);
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: pcn,
                pl: pl_push,
                push_color: Some(&red),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(all_eq(&c, 255, 0, 0, 255, 1), "cull NONE: quad drawn");
        unsafe {
            c.dev.destroy_pipeline(pcn, None);
        }
        let pcb = mkc(&c, vk::CullModeFlags::FRONT_AND_BACK, ccwf);
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: pcb,
                pl: pl_push,
                push_color: Some(&red),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 0, 0, 0, 255, 1),
            "cull FRONT_AND_BACK: nothing drawn",
        );
        unsafe {
            c.dev.destroy_pipeline(pcb, None);
        }
        let pc1 = mkc(
            &c,
            vk::CullModeFlags::BACK,
            vk::FrontFace::COUNTER_CLOCKWISE,
        );
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: pc1,
                pl: pl_push,
                push_color: Some(&red),
                vbo,
                verts: 4,
            },
            full,
        );
        let ccw = peq(&c, W / 2, H / 2, [255, 0, 0, 255], 2);
        unsafe {
            c.dev.destroy_pipeline(pc1, None);
        }
        let pc2 = mkc(&c, vk::CullModeFlags::BACK, vk::FrontFace::CLOCKWISE);
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: pc2,
                pl: pl_push,
                push_color: Some(&red),
                vbo,
                verts: 4,
            },
            full,
        );
        let cw = peq(&c, W / 2, H / 2, [255, 0, 0, 255], 2);
        unsafe {
            c.dev.destroy_pipeline(pc2, None);
        }
        s.ok(ccw != cw, "cull BACK: CCW vs CW winding flips visibility");
    }

    // Test 12: color write mask
    {
        let white = [1.0f32, 1., 1., 1.];
        let mkm = |c: &Ctx, cw| {
            mk_pipe2(
                c,
                vs_s,
                fs_s,
                pl_push,
                0,
                vk::PrimitiveTopology::TRIANGLE_STRIP,
                noblend.0,
                noblend.1,
                noblend.2,
                noblend.3,
                noblend.4,
                noblend.5,
                noblend.6,
                vk::CullModeFlags::NONE,
                ccwf,
                false,
                vk::CompareOp::ALWAYS,
                c.rp,
                cw,
            )
        };
        let pmr = mkm(&c, vk::ColorComponentFlags::R);
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: pmr,
                pl: pl_push,
                push_color: Some(&white),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 255, 0, 0, 255, 1),
            "colorWriteMask R only: white -> (255,0,0,255)",
        );
        unsafe {
            c.dev.destroy_pipeline(pmr, None);
        }
        let pma = mkm(&c, vk::ColorComponentFlags::RGBA);
        frame(
            &mut c,
            [0., 0., 0., 1.],
            Draw {
                pipe: pma,
                pl: pl_push,
                push_color: Some(&white),
                vbo,
                verts: 4,
            },
            full,
        );
        s.ok(
            all_eq(&c, 255, 255, 255, 255, 1),
            "colorWriteMask RGBA: white -> (255,255,255,255)",
        );
        unsafe {
            c.dev.destroy_pipeline(pma, None);
        }
    }

    // Test 13: format + device property queries
    {
        let fp = unsafe {
            c.inst
                .get_physical_device_format_properties(c.pd, vk::Format::R8G8B8A8_UNORM)
        };
        s.ok(
            fp.optimal_tiling_features
                .contains(vk::FormatFeatureFlags::COLOR_ATTACHMENT),
            "R8G8B8A8_UNORM optimal-tiling COLOR_ATTACHMENT",
        );
        let fpd = unsafe {
            c.inst
                .get_physical_device_format_properties(c.pd, vk::Format::D32_SFLOAT)
        };
        s.ok(
            fpd.optimal_tiling_features
                .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT),
            "D32_SFLOAT optimal-tiling DEPTH_STENCIL_ATTACHMENT",
        );
        let props = unsafe { c.inst.get_physical_device_properties(c.pd) };
        s.ok(
            vk::api_version_major(props.api_version) >= 1,
            "device apiVersion major >= 1",
        );
        s.ok(
            props.limits.max_image_dimension2_d >= W,
            "limits.maxImageDimension2D >= 64",
        );
    }

    // Test 14: 2x2 texture upload + NEAREST sampling (combined image sampler + descriptor set)
    {
        let dslb = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let dslci = vk::DescriptorSetLayoutCreateInfo::default().bindings(&dslb);
        let dsl = unsafe { c.dev.create_descriptor_set_layout(&dslci, None) }
            .expect("descriptor set layout");
        s.ok(true, "descriptor set layout");
        let set_layouts = [dsl];
        let plci = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        let pl_tex = unsafe { c.dev.create_pipeline_layout(&plci, None) }.unwrap();

        // 2x2 texture image
        let tii = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 2,
                height: 2,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let timg = unsafe { c.dev.create_image(&tii, None) }.expect("texture image");
        s.ok(true, "texture image");
        let tmr = unsafe { c.dev.get_image_memory_requirements(timg) };
        let tai = vk::MemoryAllocateInfo::default()
            .allocation_size(tmr.size)
            .memory_type_index(
                c.memtype(tmr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            );
        let tmem = unsafe { c.dev.allocate_memory(&tai, None) }.unwrap();
        unsafe { c.dev.bind_image_memory(timg, tmem, 0) }.unwrap();

        // staging buffer with 2x2 texels: red, green, blue, white (row-major)
        let texels: [u8; 16] = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let sbi = vk::BufferCreateInfo::default()
            .size(16)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC);
        let sbuf = unsafe { c.dev.create_buffer(&sbi, None) }.unwrap();
        let smr = unsafe { c.dev.get_buffer_memory_requirements(sbuf) };
        let sai = vk::MemoryAllocateInfo::default()
            .allocation_size(smr.size)
            .memory_type_index(c.memtype(
                smr.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ));
        let smem = unsafe { c.dev.allocate_memory(&sai, None) }.unwrap();
        unsafe {
            c.dev.bind_buffer_memory(sbuf, smem, 0).unwrap();
            let sp = c
                .dev
                .map_memory(smem, 0, 16, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(texels.as_ptr(), sp as *mut u8, 16);
            c.dev.unmap_memory(smem);
        }

        // upload with layout transitions
        {
            let cai = vk::CommandBufferAllocateInfo::default()
                .command_pool(c.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
            let bi = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            unsafe { c.dev.begin_command_buffer(cmd, &bi) }.unwrap();
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let b1 = [vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(timg)
                .subresource_range(range)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
            let cp = [vk::BufferImageCopy {
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_extent: vk::Extent3D {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                ..Default::default()
            }];
            let b2 = [vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(timg)
                .subresource_range(range)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)];
            unsafe {
                c.dev.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &b1,
                );
                c.dev.cmd_copy_buffer_to_image(
                    cmd,
                    sbuf,
                    timg,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &cp,
                );
                c.dev.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &b2,
                );
                c.dev.end_command_buffer(cmd).unwrap();
                let cmds = [cmd];
                let si = [vk::SubmitInfo::default().command_buffers(&cmds)];
                let fnc = c
                    .dev
                    .create_fence(&vk::FenceCreateInfo::default(), None)
                    .unwrap();
                c.dev.queue_submit(c.q, &si, fnc).unwrap();
                c.dev.wait_for_fences(&[fnc], true, u64::MAX).unwrap();
                c.dev.destroy_fence(fnc, None);
                c.dev.free_command_buffers(c.pool, &[cmd]);
            }
        }

        let tvi = vk::ImageViewCreateInfo::default()
            .image(timg)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let tview = unsafe { c.dev.create_image_view(&tvi, None) }.unwrap();
        let smci = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        let samp = unsafe { c.dev.create_sampler(&smci, None) }.unwrap();

        let dps = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
        }];
        let dpci = vk::DescriptorPoolCreateInfo::default()
            .max_sets(1)
            .pool_sizes(&dps);
        let dpool = unsafe { c.dev.create_descriptor_pool(&dpci, None) }.unwrap();
        let dsai = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(dpool)
            .set_layouts(&set_layouts);
        let dset = unsafe { c.dev.allocate_descriptor_sets(&dsai) }.unwrap()[0];
        let dii2 = [vk::DescriptorImageInfo {
            sampler: samp,
            image_view: tview,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let wds = [vk::WriteDescriptorSet::default()
            .dst_set(dset)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&dii2)];
        unsafe {
            c.dev.update_descriptor_sets(&wds, &[]);
        }

        let pt = mk_pipe2(
            &c,
            vs_tx,
            fs_tx,
            pl_tex,
            3,
            vk::PrimitiveTopology::TRIANGLE_STRIP,
            noblend.0,
            noblend.1,
            noblend.2,
            noblend.3,
            noblend.4,
            noblend.5,
            noblend.6,
            vk::CullModeFlags::NONE,
            ccwf,
            false,
            vk::CompareOp::ALWAYS,
            c.rp,
            vk::ColorComponentFlags::RGBA,
        );
        s.ok(
            dsl != vk::DescriptorSetLayout::null()
                && pl_tex != vk::PipelineLayout::null()
                && pt != vk::Pipeline::null()
                && samp != vk::Sampler::null(),
            "texture pipeline + descriptor created",
        );

        let tq: [f32; 16] = [
            -1., -1., 0., 0., 1., -1., 1., 0., -1., 1., 0., 1., 1., 1., 1., 1.,
        ];
        let (tvbo, tqm) = c.mk_vbo(&tq);

        // draw textured quad with descriptor set bound
        {
            let cai = vk::CommandBufferAllocateInfo::default()
                .command_pool(c.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
            let bi = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            unsafe { c.dev.begin_command_buffer(cmd, &bi) }.unwrap();
            let cv = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0., 0., 0., 1.],
                },
            }];
            let rpb = vk::RenderPassBeginInfo::default()
                .render_pass(c.rp)
                .framebuffer(c.fb)
                .render_area(full)
                .clear_values(&cv);
            unsafe {
                c.dev
                    .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE);
                c.dev
                    .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pt);
                c.dev.cmd_set_scissor(cmd, 0, &[full]);
                c.dev.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    pl_tex,
                    0,
                    &[dset],
                    &[],
                );
                c.dev.cmd_bind_vertex_buffers(cmd, 0, &[tvbo], &[0]);
                c.dev.cmd_draw(cmd, 4, 1, 0, 0);
                c.dev.cmd_end_render_pass(cmd);
            }
            copy_and_submit(&mut c, cmd);
        }
        s.ok(
            peq(&c, W / 4, H / 4, [255, 0, 0, 255], 2),
            "texture NEAREST top-left red",
        );
        s.ok(
            peq(&c, 3 * W / 4, H / 4, [0, 255, 0, 255], 2),
            "texture NEAREST top-right green",
        );
        s.ok(
            peq(&c, W / 4, 3 * H / 4, [0, 0, 255, 255], 2),
            "texture NEAREST bottom-left blue",
        );
        s.ok(
            peq(&c, 3 * W / 4, 3 * H / 4, [255, 255, 255, 255], 2),
            "texture NEAREST bottom-right white",
        );

        unsafe {
            c.dev.destroy_pipeline(pt, None);
            c.dev.destroy_buffer(tvbo, None);
            c.dev.free_memory(tqm, None);
            c.dev.destroy_sampler(samp, None);
            c.dev.destroy_image_view(tview, None);
            c.dev.destroy_image(timg, None);
            c.dev.free_memory(tmem, None);
            c.dev.destroy_buffer(sbuf, None);
            c.dev.free_memory(smem, None);
            c.dev.destroy_descriptor_pool(dpool, None);
            c.dev.destroy_descriptor_set_layout(dsl, None);
            c.dev.destroy_pipeline_layout(pl_tex, None);
        }
    }

    // negative control
    frame(
        &mut c,
        [0., 0., 0., 1.],
        Draw {
            pipe: pipe_solid,
            pl: pl_push,
            push_color: Some(&[1., 0., 0., 1.]),
            vbo,
            verts: 4,
        },
        full,
    );
    s.ok(
        !all_eq(&c, 0, 255, 0, 255, 2),
        "negative control: red buffer is NOT green",
    );
    s.ok(
        !peq(&c, 0, 0, [0, 0, 0, 255], 2),
        "negative control: red pixel is NOT black",
    );

    unsafe {
        c.dev.device_wait_idle().unwrap();
    }

    // teardown of long-lived vbos/memory (best-effort; process exits right after)
    unsafe {
        c.dev.destroy_buffer(vbo, None);
        c.dev.free_memory(qm, None);
        c.dev.destroy_buffer(gvbo, None);
        c.dev.free_memory(gm, None);
    }

    let expected = 68;
    let total = s.pass + s.fail;
    println!(
        "vulkan-render-rust: PASS={} FAIL={} TOTAL={} EXPECTED={}",
        s.pass, s.fail, total, expected
    );
    if s.fail == 0 && total == expected {
        println!("VULKAN_RENDER_RUST_FULL_API OK {}", s.pass);
        std::process::exit(0);
    }
    std::process::exit(1);
}
