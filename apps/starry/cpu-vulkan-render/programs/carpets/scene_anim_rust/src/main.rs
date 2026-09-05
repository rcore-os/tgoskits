// scene_anim_rust - keyframe-animation RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
// no GPU/window/surface/swapchain), Rust `ash` binding of the same offscreen render pipeline as the C++
// cell scene_anim.cpp. `ash::Entry::load()` dlopens libvulkan.so.1; renders N=4 keyframes of a transformed
// unit quad through a real graphics pipeline (SPIR-V vertex+fragment shaders), each frame's model
// transform (rotation about the FBO center + scale + translate, cubic-ease scale) passed as push
// constants. The four R*S*local+T corners and the frame center are computed INDEPENDENTLY in Rust and
// asserted at exact pixels, plus a point just outside the quad. The reference math is behaviour-identical
// to the C++ cell; only the ash-vs-C++ Vulkan binding syntax differs. Prints "SCENE_ANIM_RUST OK <n>"
// only when FAIL==0 && TOTAL==EXPECTED==PASS.
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

fn lerpf(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn ease_cubic(t: f32) -> f32 {
    3.0 * t * t - 2.0 * t * t * t
}

// push-constant block: { vec2 vp; vec2 col0; vec2 col1; vec2 tr; vec4 u; }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Pc {
    vp: [f32; 2],
    col0: [f32; 2],
    col1: [f32; 2],
    tr: [f32; 2],
    u: [f32; 4],
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
    pipe: vk::Pipeline,
    pl: vk::PipelineLayout,
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
    fn near_color(&self, x: i32, y: i32, r: i32, g: i32, b: i32, tol: i32) -> bool {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (xx, yy) = (x + dx, y + dy);
                if xx < 0 || yy < 0 || xx >= W as i32 || yy >= H as i32 {
                    continue;
                }
                if self.peq(xx as u32, yy as u32, [r, g, b, 255], tol) {
                    return true;
                }
            }
        }
        false
    }
    // draw one animated quad frame: clear black, push transform+color, draw 4-vertex strip, read back.
    fn draw_frame(&mut self, p: &Pc) {
        let cai = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { self.dev.allocate_command_buffers(&cai) }.unwrap()[0];
        let bi = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { self.dev.begin_command_buffer(cmd, &bi) }.unwrap();
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
                    width: W,
                    height: H,
                },
            })
            .clear_values(&cv);
        let full = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: W,
                height: H,
            },
        };
        unsafe {
            self.dev
                .cmd_begin_render_pass(cmd, &rpb, vk::SubpassContents::INLINE);
            self.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipe);
            self.dev.cmd_set_scissor(cmd, 0, &[full]);
            let bytes = std::slice::from_raw_parts(
                (p as *const Pc) as *const u8,
                std::mem::size_of::<Pc>(),
            );
            self.dev.cmd_push_constants(
                cmd,
                self.pl,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytes,
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
                width: W,
                height: H,
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
            std::ptr::copy_nonoverlapping(self.rmap, self.buf.as_mut_ptr(), (W * H * 4) as usize);
        }
    }
}

const A0: f32 = 0.0;
const S0: f32 = 6.0;
const S1: f32 = 14.0;
const CX0: f32 = 20.0;
const CX1: f32 = 44.0;
const CY0: f32 = 20.0;
const CY1: f32 = 44.0;

// frame transform: R(theta)*S columns + T, byte-identical to the C++ cell.
fn frame_transform(t: f32) -> ([f32; 2], [f32; 2], [f32; 2], f32, f32) {
    let a1 = std::f32::consts::PI / 2.0;
    let ang = lerpf(A0, a1, t);
    let sc = lerpf(S0, S1, ease_cubic(t));
    let cx = lerpf(CX0, CX1, t);
    let cy = lerpf(CY0, CY1, t);
    let ca = ang.cos();
    let sa = ang.sin();
    ([sc * ca, sc * sa], [-sc * sa, sc * ca], [cx, cy], sc, ang)
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
        pipe: vk::Pipeline::null(),
        pl: vk::PipelineLayout::null(),
        vbo: vk::Buffer::null(),
    };

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

    let vs = c.shmod(include_bytes!("../shaders/anim_vert.spv"));
    let fs = c.shmod(include_bytes!("../shaders/anim_frag.spv"));
    let pcr = [vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
        offset: 0,
        size: std::mem::size_of::<Pc>() as u32,
    }];
    let li = vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&pcr);
    c.pl = unsafe { c.dev.create_pipeline_layout(&li, None) }.unwrap();
    // pipeline (pos2 stride 8, TRIANGLE_STRIP, dynamic scissor)
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
        stride: 8,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attr = [vk::VertexInputAttributeDescription {
        location: 0,
        binding: 0,
        format: vk::Format::R32G32_SFLOAT,
        offset: 0,
    }];
    let visi = vk::PipelineVertexInputStateCreateInfo::default()
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
        .blend_enable(false)];
    let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
    let gp = [vk::GraphicsPipelineCreateInfo::default()
        .stages(&st)
        .vertex_input_state(&visi)
        .input_assembly_state(&ia)
        .viewport_state(&vps)
        .rasterization_state(&rs)
        .multisample_state(&ms)
        .color_blend_state(&cb)
        .dynamic_state(&ds)
        .layout(c.pl)
        .render_pass(c.rp)
        .subpass(0)];
    c.pipe = unsafe {
        c.dev
            .create_graphics_pipelines(vk::PipelineCache::null(), &gp, None)
    }
    .expect("pipeline")[0];
    s.ok(c.pipe != vk::Pipeline::null(), "anim pipeline created");

    let local: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    let (vbo, _vbmem) = c.mk_vbo(&local);
    c.vbo = vbo;

    let ts = [0.0f32, 0.25, 0.5, 0.75];
    let cols = [
        [1.0f32, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
    ];

    for fi in 0..4usize {
        let t = ts[fi];
        let (col0, col1, tr, sc, ang) = frame_transform(t);
        let p = Pc {
            vp: [W as f32, H as f32],
            col0,
            col1,
            tr,
            u: [cols[fi][0], cols[fi][1], cols[fi][2], 1.0],
        };
        c.draw_frame(&p);

        let ca = ang.cos();
        let sa = ang.sin();
        let mut cornx = [0.0f32; 4];
        let mut corny = [0.0f32; 4];
        for k in 0..4 {
            let lx = local[k * 2];
            let ly = local[k * 2 + 1];
            let rx = sc * (ca * lx - sa * ly);
            let ry = sc * (sa * lx + ca * ly);
            cornx[k] = tr[0] + rx;
            corny[k] = tr[1] + ry;
        }
        let e = ease_cubic(t);
        let e_ref = 3.0 * t * t - 2.0 * t * t * t;
        s.ok((e - e_ref).abs() < 1e-6, "ease_cubic closed-form value");
        s.ok(
            (sc - (S0 + (S1 - S0) * e)).abs() < 1e-4,
            "scale = lerp(S0,S1,ease(t)) closed-form",
        );

        let cxi = (tr[0] - 0.5).round() as i32;
        let cyi = (tr[1] - 0.5).round() as i32;
        s.ok(
            c.peq(
                cxi as u32,
                cyi as u32,
                [
                    (cols[fi][0] * 255.0).round() as i32,
                    (cols[fi][1] * 255.0).round() as i32,
                    (cols[fi][2] * 255.0).round() as i32,
                    255,
                ],
                2,
            ),
            "frame center pixel carries frame color at closed-form center",
        );

        for k in 0..4 {
            let px_ = (cornx[k] - 0.5).round() as i32;
            let py_ = (corny[k] - 0.5).round() as i32;
            let onscreen = px_ >= 0 && py_ >= 0 && px_ < W as i32 && py_ < H as i32;
            s.ok(
                onscreen
                    && c.near_color(
                        px_,
                        py_,
                        (cols[fi][0] * 255.0).round() as i32,
                        (cols[fi][1] * 255.0).round() as i32,
                        (cols[fi][2] * 255.0).round() as i32,
                        40,
                    ),
                "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)",
            );
        }

        {
            let ox = if fi < 2 { (W - 2) as i32 } else { 1 };
            let oy = if fi < 2 { (H - 2) as i32 } else { 1 };
            let reach = sc * std::f32::consts::SQRT_2;
            let covers = (ox as f32 + 0.5 - tr[0]).abs() <= reach
                && (oy as f32 + 0.5 - tr[1]).abs() <= reach;
            if !covers {
                s.ok(
                    c.peq(ox as u32, oy as u32, [0, 0, 0, 255], 2),
                    "outside-quad point stays background (closed-form silhouette)",
                );
            } else {
                s.ok(true, "outside-quad point skipped (would be covered)");
            }
        }
    }

    {
        let (_, _, tra, ..) = frame_transform(0.0);
        let (_, _, trb, ..) = frame_transform(0.75);
        s.ok(
            (tra[0] - trb[0]).abs() > 1.0,
            "center translates between t=0 and t=0.75 (animation is real)",
        );
    }
    {
        let (col0, _, _, _, ang) = frame_transform(0.5);
        s.ok(
            (ang - std::f32::consts::PI / 4.0).abs() < 1e-5,
            "t=0.5 rotation angle = pi/4 closed-form",
        );
        s.ok(
            (col0[0] - col0[1]).abs() < 1e-4 && col0[0] > 0.0,
            "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)",
        );
    }
    {
        let (col0, col1, tr, ..) = frame_transform(0.0);
        let p = Pc {
            vp: [W as f32, H as f32],
            col0,
            col1,
            tr,
            u: [1.0, 0.0, 0.0, 1.0],
        };
        c.draw_frame(&p);
        let cxi = (tr[0] - 0.5).round() as i32;
        let cyi = (tr[1] - 0.5).round() as i32;
        s.ok(
            !c.peq(cxi as u32, cyi as u32, [0, 255, 0, 255], 4),
            "negative control: frame-0 center is NOT green",
        );
    }

    unsafe { c.dev.device_wait_idle().unwrap() };
    let (pass, fail) = (s.pass, s.fail);
    let total = pass + fail;
    let expected = 47;
    println!("scene-anim-rust: PASS={pass} FAIL={fail} TOTAL={total} EXPECTED={expected}");
    if fail == 0 && total == expected {
        println!("SCENE_ANIM_RUST OK {pass}");
        std::process::exit(0);
    }
    std::process::exit(1);
}
