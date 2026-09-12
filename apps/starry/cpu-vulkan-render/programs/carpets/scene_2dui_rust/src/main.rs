// scene_2dui_rust - 2D UI compositing RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
// no GPU/window/surface/swapchain), Rust `ash` binding of the same offscreen render pipeline as the C++
// cell scene_2dui.cpp. `ash::Entry::load()` dlopens libvulkan.so.1; the cell builds an offscreen render
// pass into an R8G8B8A8_UNORM color image, draws through real graphics pipelines (SPIR-V vertex+fragment
// shaders loaded via read_spv), copies the image to a host-visible buffer with cmd_copy_image_to_buffer,
// maps it, and checks every pixel against a closed-form reference: filled rectangles, an analytic
// rounded-rect (fragment-shader discard vs the identical closed form), a nine-patch border frame, an 8x8
// bitmap-font glyph blit (VkImage + NEAREST combined image sampler, all 64 texels), a scissor-clipped
// fill, and MULTI-LAYER Porter-Duff over compositing Co = Cs*As + Cd*(1-As). The closed-form reference is
// behaviour-identical to the C++ cell; only the ash-vs-C++ Vulkan binding syntax differs. Prints
// "SCENE_2DUI_RUST OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
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

fn clampi(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}
fn q8(f: f32) -> i32 {
    clampi((f * 255.0).round() as i32, 0, 255)
}

// push-constant block shared vertex+fragment: { vec2 vp; vec2 pad; vec4 col; vec4 box; float rad; vec3 pad }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Pc {
    vp: [f32; 2],
    _pad0: [f32; 2],
    col: [f32; 4],
    boxv: [f32; 4],
    rad: f32,
    _pad1: [f32; 3],
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
    cview: vk::ImageView,
    rp: vk::RenderPass,
    fb: vk::Framebuffer,
    rbuf: vk::Buffer,
    rmap: *mut u8,
    buf: Vec<u8>,
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
    fn px(&self, x: u32, y: u32, c: u32) -> i32 {
        self.buf[((y * W + x) * 4 + c) as usize] as i32
    }
    fn peq(&self, x: u32, y: u32, rgba: [i32; 4], tol: i32) -> bool {
        (self.px(x, y, 0) - rgba[0]).abs() <= tol
            && (self.px(x, y, 1) - rgba[1]).abs() <= tol
            && (self.px(x, y, 2) - rgba[2]).abs() <= tol
            && (self.px(x, y, 3) - rgba[3]).abs() <= tol
    }
}

// pipeline. vlayout 0 = pos2 (stride 8), 1 = pos2+uv (stride 16). blend toggles SRC_ALPHA over.
fn mk_pipe(
    c: &Ctx,
    vs: vk::ShaderModule,
    fs: vk::ShaderModule,
    pl: vk::PipelineLayout,
    vlayout: u32,
    blend: bool,
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
    let stride = if vlayout == 1 { 16 } else { 8 };
    let bind = [vk::VertexInputBindingDescription {
        binding: 0,
        stride,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let mut attr = vec![vk::VertexInputAttributeDescription {
        location: 0,
        binding: 0,
        format: vk::Format::R32G32_SFLOAT,
        offset: 0,
    }];
    if vlayout == 1 {
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
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
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
        .cull_mode(vk::CullModeFlags::NONE)
        .line_width(1.0);
    let ms = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let cba = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .blend_enable(blend)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)];
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
        .render_pass(c.rp)
        .subpass(0)];
    unsafe {
        c.dev
            .create_graphics_pipelines(vk::PipelineCache::null(), &gp, None)
    }
    .expect("pipeline")[0]
}

// A recording context: begin the offscreen pass with a clear color, let the caller record draws, then
// copy the image into the readback buffer, submit+wait, and capture px.
fn begin_frame(c: &Ctx, clear: [f32; 4]) -> vk::CommandBuffer {
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
            .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE)
    };
    cmd
}
fn end_frame(c: &mut Ctx, cmd: vk::CommandBuffer) {
    unsafe { c.dev.cmd_end_render_pass(cmd) };
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
        std::ptr::copy_nonoverlapping(c.rmap, c.buf.as_mut_ptr(), (W * H * 4) as usize);
    }
}

fn rect_verts(x0: f32, y0: f32, x1: f32, y1: f32) -> [f32; 12] {
    [x0, y0, x1, y0, x0, y1, x0, y1, x1, y0, x1, y1]
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
        rbuf: vk::Buffer::null(),
        rmap: std::ptr::null_mut(),
        buf: vec![0u8; (W * H * 4) as usize],
    };

    // color image (R8G8B8A8_UNORM)
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
    s.ok(c.cimg != vk::Image::null(), "vkCreateImage color");
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
    s.ok(c.cview != vk::ImageView::null(), "vkCreateImageView");

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
    s.ok(c.rp != vk::RenderPass::null(), "vkCreateRenderPass");
    let fbv = [c.cview];
    let fbi = vk::FramebufferCreateInfo::default()
        .render_pass(c.rp)
        .attachments(&fbv)
        .width(W)
        .height(H)
        .layers(1);
    c.fb = unsafe { c.dev.create_framebuffer(&fbi, None) }.expect("framebuffer");
    s.ok(c.fb != vk::Framebuffer::null(), "vkCreateFramebuffer");

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
    s.ok(c.pool != vk::CommandPool::null(), "vkCreateCommandPool");
    s.ok(true, "offscreen R8G8B8A8 target + readback buffer ready");

    // shaders + layouts + pipelines
    let vs_pix = c.shmod(include_bytes!("../shaders/pix_vert.spv"));
    let fs_uni = c.shmod(include_bytes!("../shaders/uni_frag.spv"));
    let fs_rr = c.shmod(include_bytes!("../shaders/rr_frag.spv"));
    let vs_tex = c.shmod(include_bytes!("../shaders/tex_vert.spv"));
    let fs_tex = c.shmod(include_bytes!("../shaders/tex_frag.spv"));
    let pcr = [vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
        offset: 0,
        size: std::mem::size_of::<Pc>() as u32,
    }];
    let li = vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&pcr);
    let pl_pc = unsafe { c.dev.create_pipeline_layout(&li, None) }.expect("pc layout");
    let pipe_uni = mk_pipe(&c, vs_pix, fs_uni, pl_pc, 0, false);
    let pipe_blend = mk_pipe(&c, vs_pix, fs_uni, pl_pc, 0, true);
    let pipe_rr = mk_pipe(&c, vs_pix, fs_rr, pl_pc, 0, false);
    s.ok(
        [pipe_uni, pipe_blend, pipe_rr]
            .iter()
            .all(|p| *p != vk::Pipeline::null()),
        "pixel-fill / blend / rounded-rect pipelines created",
    );
    let full = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D {
            width: W,
            height: H,
        },
    };
    let base_pc = Pc {
        vp: [W as f32, H as f32],
        ..Default::default()
    };

    let push = |c: &Ctx, cmd: vk::CommandBuffer, p: &Pc| unsafe {
        let bytes =
            std::slice::from_raw_parts((p as *const Pc) as *const u8, std::mem::size_of::<Pc>());
        c.dev.cmd_push_constants(
            cmd,
            pl_pc,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytes,
        );
    };

    // ---- Scene A: filled rectangles ----
    {
        let cmd = begin_frame(&c, [0.0, 0.0, 0.0, 1.0]);
        unsafe {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe_uni);
            c.dev.cmd_set_scissor(cmd, 0, &[full]);
        }
        let va = rect_verts(8.0, 8.0, 16.0, 24.0);
        let (ba, ma) = c.mk_vbo(&va);
        let mut p = base_pc;
        p.col = [1.0, 0.0, 0.0, 1.0];
        push(&c, cmd, &p);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[ba], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        let vb = rect_verts(40.0, 32.0, 48.0, 52.0);
        let (bb, mb) = c.mk_vbo(&vb);
        let mut p2 = base_pc;
        p2.col = [0.0, 1.0, 0.0, 1.0];
        push(&c, cmd, &p2);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[bb], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        end_frame(&mut c, cmd);
        unsafe {
            c.dev.destroy_buffer(ba, None);
            c.dev.free_memory(ma, None);
            c.dev.destroy_buffer(bb, None);
            c.dev.free_memory(mb, None);
        }
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let (er, eg, eb);
                if (8..16).contains(&x) && (8..24).contains(&y) {
                    er = 255;
                    eg = 0;
                    eb = 0;
                } else if (40..48).contains(&x) && (32..52).contains(&y) {
                    er = 0;
                    eg = 255;
                    eb = 0;
                } else {
                    er = 0;
                    eg = 0;
                    eb = 0;
                }
                if !c.peq(x, y, [er, eg, eb, 255], 1) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "filled rectangles: every pixel matches closed-form rect coverage",
        );
        s.ok(c.peq(10, 10, [255, 0, 0, 255], 1), "rect A interior red");
        s.ok(c.peq(44, 40, [0, 255, 0, 255], 1), "rect B interior green");
        s.ok(
            c.peq(30, 30, [0, 0, 0, 255], 1),
            "gap between rects is background",
        );
    }

    // ---- Scene B: analytic rounded-rect ----
    {
        let cmd = begin_frame(&c, [0.0, 0.0, 0.0, 1.0]);
        unsafe {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe_rr);
            c.dev.cmd_set_scissor(cmd, 0, &[full]);
        }
        let mut p = base_pc;
        p.col = [1.0, 1.0, 0.0, 1.0];
        p.boxv = [12.0, 12.0, 52.0, 52.0];
        p.rad = 8.0;
        push(&c, cmd, &p);
        let fq: [f32; 12] = [
            0.0, 0.0, W as f32, 0.0, 0.0, H as f32, 0.0, H as f32, W as f32, 0.0, W as f32,
            H as f32,
        ];
        let (fbo2, fm) = c.mk_vbo(&fq);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[fbo2], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        end_frame(&mut c, cmd);
        unsafe {
            c.dev.destroy_buffer(fbo2, None);
            c.dev.free_memory(fm, None);
        }
        let covered = |x: u32, y: u32| -> bool {
            let cx = x as f32 + 0.5;
            let cy = y as f32 + 0.5;
            let (x0, y0, x1, y1, rr) = (12.0f32, 12.0f32, 52.0f32, 52.0f32, 8.0f32);
            if !(cx >= x0 && cx < x1 && cy >= y0 && cy < y1) {
                return false;
            }
            let (mut ccx, mut ccy) = (0.0f32, 0.0f32);
            let mut corner = false;
            if cx < x0 + rr && cy < y0 + rr {
                corner = true;
                ccx = x0 + rr;
                ccy = y0 + rr;
            } else if cx >= x1 - rr && cy < y0 + rr {
                corner = true;
                ccx = x1 - rr;
                ccy = y0 + rr;
            } else if cx < x0 + rr && cy >= y1 - rr {
                corner = true;
                ccx = x0 + rr;
                ccy = y1 - rr;
            } else if cx >= x1 - rr && cy >= y1 - rr {
                corner = true;
                ccx = x1 - rr;
                ccy = y1 - rr;
            }
            if corner {
                let dx = cx - ccx;
                let dy = cy - ccy;
                if (dx * dx + dy * dy).sqrt() > rr {
                    return false;
                }
            }
            true
        };
        let mut bad = 0;
        let mut lit = 0;
        for y in 0..H {
            for x in 0..W {
                let cov = covered(x, y);
                if cov {
                    lit += 1;
                }
                let (er, eg, eb) = if cov { (255, 255, 0) } else { (0, 0, 0) };
                if !c.peq(x, y, [er, eg, eb, 255], 1) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "rounded-rect: every pixel matches analytic corner-arc coverage",
        );
        s.ok(lit > 0, "rounded-rect: some pixels covered");
        s.ok(
            c.peq(32, 32, [255, 255, 0, 255], 1),
            "rounded-rect center lit",
        );
        s.ok(
            c.peq(12, 12, [0, 0, 0, 255], 1),
            "rounded-rect clipped corner (12,12) is background",
        );
        s.ok(
            c.peq(32, 13, [255, 255, 0, 255], 1),
            "rounded-rect straight top edge lit",
        );
    }

    // ---- Scene C: nine-patch-style scaled border frame ----
    {
        let cmd = begin_frame(&c, [0.0, 0.0, 0.0, 1.0]);
        unsafe {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe_uni);
            c.dev.cmd_set_scissor(cmd, 0, &[full]);
        }
        let vo = rect_verts(4.0, 4.0, 60.0, 60.0);
        let (bo, mo) = c.mk_vbo(&vo);
        let mut p = base_pc;
        p.col = [0.0, 0.0, 1.0, 1.0];
        push(&c, cmd, &p);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[bo], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        let vin = rect_verts(10.0, 10.0, 54.0, 54.0);
        let (bin, mi) = c.mk_vbo(&vin);
        let mut p2 = base_pc;
        p2.col = [0.1, 0.1, 0.1, 1.0];
        push(&c, cmd, &p2);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[bin], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        end_frame(&mut c, cmd);
        unsafe {
            c.dev.destroy_buffer(bo, None);
            c.dev.free_memory(mo, None);
            c.dev.destroy_buffer(bin, None);
            c.dev.free_memory(mi, None);
        }
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let inbox = (4..60).contains(&x) && (4..60).contains(&y);
                let ininner = (10..54).contains(&x) && (10..54).contains(&y);
                let (er, eg, eb);
                if ininner {
                    er = q8(0.1);
                    eg = q8(0.1);
                    eb = q8(0.1);
                } else if inbox {
                    er = 0;
                    eg = 0;
                    eb = 255;
                } else {
                    er = 0;
                    eg = 0;
                    eb = 0;
                }
                if !c.peq(x, y, [er, eg, eb, 255], 1) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "nine-patch border frame: closed-form border-vs-interior coverage",
        );
        s.ok(
            c.peq(5, 32, [0, 0, 255, 255], 1),
            "nine-patch left border blue",
        );
        s.ok(
            c.peq(32, 5, [0, 0, 255, 255], 1),
            "nine-patch top border blue",
        );
        s.ok(
            c.peq(32, 32, [q8(0.1), q8(0.1), q8(0.1), 255], 1),
            "nine-patch hollow interior",
        );
    }

    // ---- Scene D: 8x8 bitmap-font glyph blit ----
    let glyph_h: [u8; 8] = [0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00];
    {
        let mut rgba = [0u8; 8 * 8 * 4];
        for (rr, &row) in glyph_h.iter().enumerate() {
            for cc in 0..8 {
                let lit = (row >> (7 - cc)) & 1 != 0;
                let v = if lit { 255u8 } else { 0u8 };
                let idx = (rr * 8 + cc) * 4;
                rgba[idx] = v;
                rgba[idx + 1] = v;
                rgba[idx + 2] = v;
                rgba[idx + 3] = 255;
            }
        }
        // glyph texture: 8x8 R8G8B8A8, NEAREST, staged upload with layout barriers.
        let tii = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 8,
                height: 8,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let gtex = unsafe { c.dev.create_image(&tii, None) }.expect("glyph image");
        s.ok(gtex != vk::Image::null(), "glyph image");
        let tmr = unsafe { c.dev.get_image_memory_requirements(gtex) };
        let tai = vk::MemoryAllocateInfo::default()
            .allocation_size(tmr.size)
            .memory_type_index(
                c.memtype(tmr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            );
        let tmem = unsafe { c.dev.allocate_memory(&tai, None) }.unwrap();
        unsafe { c.dev.bind_image_memory(gtex, tmem, 0) }.unwrap();
        let sbi = vk::BufferCreateInfo::default()
            .size(rgba.len() as u64)
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
            let p = c
                .dev
                .map_memory(smem, 0, rgba.len() as u64, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), p as *mut u8, rgba.len());
            c.dev.unmap_memory(smem);
        }
        {
            let cai = vk::CommandBufferAllocateInfo::default()
                .command_pool(c.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
            let bi = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            unsafe { c.dev.begin_command_buffer(cmd, &bi) }.unwrap();
            let b1 = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(gtex)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            unsafe {
                c.dev.cmd_pipeline_barrier(
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
                    width: 8,
                    height: 8,
                    depth: 1,
                },
                ..Default::default()
            };
            unsafe {
                c.dev.cmd_copy_buffer_to_image(
                    cmd,
                    sbuf,
                    gtex,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[cp],
                )
            };
            let b2 = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(gtex)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ);
            unsafe {
                c.dev.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[b2],
                );
                c.dev.end_command_buffer(cmd).unwrap();
            }
            let cmds = [cmd];
            let si = [vk::SubmitInfo::default().command_buffers(&cmds)];
            let fnc = unsafe { c.dev.create_fence(&vk::FenceCreateInfo::default(), None) }.unwrap();
            unsafe {
                c.dev.queue_submit(c.q, &si, fnc).unwrap();
                c.dev.wait_for_fences(&[fnc], true, u64::MAX).unwrap();
                c.dev.destroy_fence(fnc, None);
                c.dev.free_command_buffers(c.pool, &[cmd]);
            }
        }
        let tvi = vk::ImageViewCreateInfo::default()
            .image(gtex)
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
        let dslb = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let dslci = vk::DescriptorSetLayoutCreateInfo::default().bindings(&dslb);
        let dsl = unsafe { c.dev.create_descriptor_set_layout(&dslci, None) }.expect("glyph dsl");
        s.ok(
            dsl != vk::DescriptorSetLayout::null(),
            "glyph descriptor set layout",
        );
        let tpcr = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX,
            offset: 0,
            size: 16,
        }];
        let sls = [dsl];
        let plci = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&sls)
            .push_constant_ranges(&tpcr);
        let pl_tex = unsafe { c.dev.create_pipeline_layout(&plci, None) }.unwrap();
        let pt = mk_pipe(&c, vs_tex, fs_tex, pl_tex, 1, false);
        s.ok(
            pt != vk::Pipeline::null() && samp != vk::Sampler::null(),
            "glyph pipeline + descriptor created",
        );
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
            .set_layouts(&sls);
        let dset = unsafe { c.dev.allocate_descriptor_sets(&dsai) }.unwrap()[0];
        let dii = [vk::DescriptorImageInfo {
            sampler: samp,
            image_view: tview,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let wds = [vk::WriteDescriptorSet::default()
            .dst_set(dset)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&dii)];
        unsafe { c.dev.update_descriptor_sets(&wds, &[]) };
        let gq: [f32; 24] = [
            20.0, 20.0, 0.0, 0.0, 28.0, 20.0, 1.0, 0.0, 20.0, 28.0, 0.0, 1.0, 20.0, 28.0, 0.0, 1.0,
            28.0, 20.0, 1.0, 0.0, 28.0, 28.0, 1.0, 1.0,
        ];
        let (gvbo, gm) = c.mk_vbo(&gq);
        {
            let cmd = begin_frame(&c, [0.0, 0.0, 0.0, 1.0]);
            unsafe {
                c.dev
                    .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pt);
                c.dev.cmd_set_scissor(cmd, 0, &[full]);
                let vpv: [f32; 2] = [W as f32, H as f32];
                let vbytes = std::slice::from_raw_parts(vpv.as_ptr() as *const u8, 8);
                c.dev
                    .cmd_push_constants(cmd, pl_tex, vk::ShaderStageFlags::VERTEX, 0, vbytes);
                c.dev.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    pl_tex,
                    0,
                    &[dset],
                    &[],
                );
                c.dev.cmd_bind_vertex_buffers(cmd, 0, &[gvbo], &[0]);
                c.dev.cmd_draw(cmd, 6, 1, 0, 0);
            }
            end_frame(&mut c, cmd);
        }
        let mut bad = 0;
        for dy in 0..8u32 {
            for dx in 0..8u32 {
                let sx = 20 + dx;
                let sy = 20 + dy;
                let lit = (glyph_h[dy as usize] >> (7 - dx)) & 1 != 0;
                let v = if lit { 255 } else { 0 };
                if !c.peq(sx, sy, [v, v, v, 255], 1) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap",
        );
        s.ok(
            c.peq(21, 23, [255, 255, 255, 255], 1),
            "glyph crossbar lit (col1,row3)",
        );
        s.ok(c.peq(23, 20, [0, 0, 0, 255], 1), "glyph row0 blank");
        s.ok(
            c.peq(24, 21, [0, 0, 0, 255], 1),
            "glyph row1 middle blank (0x42)",
        );
        unsafe {
            c.dev.destroy_pipeline(pt, None);
            c.dev.destroy_buffer(gvbo, None);
            c.dev.free_memory(gm, None);
            c.dev.destroy_sampler(samp, None);
            c.dev.destroy_image_view(tview, None);
            c.dev.destroy_image(gtex, None);
            c.dev.free_memory(tmem, None);
            c.dev.destroy_buffer(sbuf, None);
            c.dev.free_memory(smem, None);
            c.dev.destroy_descriptor_pool(dpool, None);
            c.dev.destroy_descriptor_set_layout(dsl, None);
            c.dev.destroy_pipeline_layout(pl_tex, None);
        }
    }

    // ---- Scene E: scissor-clipped fill ----
    {
        let cmd = begin_frame(&c, [0.0, 0.0, 0.0, 1.0]);
        let boxr = vk::Rect2D {
            offset: vk::Offset2D { x: 16, y: 16 },
            extent: vk::Extent2D {
                width: 20,
                height: 20,
            },
        };
        unsafe {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe_uni);
            c.dev.cmd_set_scissor(cmd, 0, &[boxr]);
        }
        let mut p = base_pc;
        p.col = [1.0, 0.0, 1.0, 1.0];
        push(&c, cmd, &p);
        let fv = rect_verts(0.0, 0.0, W as f32, H as f32);
        let (fbb, fm) = c.mk_vbo(&fv);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[fbb], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        end_frame(&mut c, cmd);
        unsafe {
            c.dev.destroy_buffer(fbb, None);
            c.dev.free_memory(fm, None);
        }
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let inr = (16..36).contains(&x) && (16..36).contains(&y);
                let (er, eg, eb) = if inr { (255, 0, 255) } else { (0, 0, 0) };
                if !c.peq(x, y, [er, eg, eb, 255], 1) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "scissor-clipped fill: magenta only within [16,36)^2",
        );
        s.ok(
            c.peq(20, 20, [255, 0, 255, 255], 1),
            "scissor inside magenta",
        );
        s.ok(
            c.peq(40, 40, [0, 0, 0, 255], 1),
            "scissor outside background",
        );
    }

    // ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
    {
        let bg = [0.10f32, 0.10, 0.10, 1.0];
        // [r,g,b,a, x0,y0,x1,y1]
        let layers: [[f32; 8]; 3] = [
            [1.0, 0.0, 0.0, 0.50, 8.0, 8.0, 56.0, 56.0],
            [0.0, 1.0, 0.0, 0.25, 12.0, 12.0, 52.0, 52.0],
            [0.0, 0.0, 1.0, 0.75, 16.0, 16.0, 48.0, 48.0],
        ];
        let cmd = begin_frame(&c, bg);
        unsafe {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe_blend);
            c.dev.cmd_set_scissor(cmd, 0, &[full]);
        }
        let mut lbm = Vec::new();
        for l in &layers {
            let lv = rect_verts(l[4], l[5], l[6], l[7]);
            let (lb, lm) = c.mk_vbo(&lv);
            let mut p = base_pc;
            p.col = [l[0], l[1], l[2], l[3]];
            push(&c, cmd, &p);
            unsafe {
                c.dev.cmd_bind_vertex_buffers(cmd, 0, &[lb], &[0]);
                c.dev.cmd_draw(cmd, 6, 1, 0, 0);
            }
            lbm.push((lb, lm));
        }
        end_frame(&mut c, cmd);
        for (lb, lm) in lbm {
            unsafe {
                c.dev.destroy_buffer(lb, None);
                c.dev.free_memory(lm, None);
            }
        }
        let composite = |tx: u32, ty: u32| -> [f32; 4] {
            let mut cc = bg;
            for l in &layers {
                let cx = tx as f32 + 0.5;
                let cy = ty as f32 + 0.5;
                if cx >= l[4] && cx < l[6] && cy >= l[5] && cy < l[7] {
                    let as_ = l[3];
                    let src = [l[0], l[1], l[2], l[3]];
                    for k in 0..4 {
                        cc[k] = src[k] * as_ + cc[k] * (1.0 - as_);
                    }
                }
            }
            cc
        };
        let mut bad = 0;
        for y in 0..H {
            for x in 0..W {
                let e = composite(x, y);
                if !c.peq(x, y, [q8(e[0]), q8(e[1]), q8(e[2]), q8(e[3])], 2) {
                    bad += 1;
                }
            }
        }
        s.ok(
            bad == 0,
            "multi-layer over: every pixel matches Porter-Duff over accumulation (incl \
             partial-overlap regions)",
        );
        {
            let mut cc = bg;
            let ls = [
                [1.0f32, 0.0, 0.0, 0.5],
                [0.0, 1.0, 0.0, 0.25],
                [0.0, 0.0, 1.0, 0.75],
            ];
            for li in &ls {
                let as_ = li[3];
                for k in 0..4 {
                    cc[k] = li[k] * as_ + cc[k] * (1.0 - as_);
                }
            }
            s.ok(
                c.peq(32, 32, [q8(cc[0]), q8(cc[1]), q8(cc[2]), q8(cc[3])], 2),
                "multi-layer over center pixel matches hand-iterated over",
            );
        }
        {
            let as_ = 0.5f32;
            let er = 1.0 * as_ + bg[0] * (1.0 - as_);
            let eg = 0.0 * as_ + bg[1] * (1.0 - as_);
            let eb = 0.0 * as_ + bg[2] * (1.0 - as_);
            let ea = as_ * as_ + bg[3] * (1.0 - as_);
            s.ok(
                c.peq(10, 32, [q8(er), q8(eg), q8(eb), q8(ea)], 2),
                "multi-layer over: single-layer region matches one over",
            );
        }
    }

    // ---- Negative control ----
    {
        let cmd = begin_frame(&c, [0.0, 0.0, 0.0, 1.0]);
        unsafe {
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe_uni);
            c.dev.cmd_set_scissor(cmd, 0, &[full]);
        }
        let va = rect_verts(8.0, 8.0, 16.0, 24.0);
        let (ba, ma) = c.mk_vbo(&va);
        let mut p = base_pc;
        p.col = [1.0, 0.0, 0.0, 1.0];
        push(&c, cmd, &p);
        unsafe {
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[ba], &[0]);
            c.dev.cmd_draw(cmd, 6, 1, 0, 0);
        }
        end_frame(&mut c, cmd);
        unsafe {
            c.dev.destroy_buffer(ba, None);
            c.dev.free_memory(ma, None);
        }
    }
    s.ok(
        !c.peq(10, 10, [0, 255, 0, 255], 4),
        "negative control: red rect pixel is NOT green",
    );
    s.ok(
        !c.peq(30, 30, [255, 0, 0, 255], 4),
        "negative control: background is NOT red",
    );

    unsafe { c.dev.device_wait_idle().unwrap() };
    let (pass, fail) = (s.pass, s.fail);
    let total = pass + fail;
    let expected = 39;
    println!("scene-2dui-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_2DUI_RUST OK {pass}");
        std::process::exit(0);
    }
    std::process::exit(1);
}
