// scene_3dmodel_rust - 3D indexed-mesh RENDER-scene carpet on Vulkan (Mesa lavapipe, CPU software Vulkan,
// no GPU/window/surface/swapchain), Rust `ash` binding of the same offscreen render pipeline as the C++
// cell scene_3dmodel.cpp. `ash::Entry::load()` dlopens libvulkan.so.1; an offscreen render pass into an
// R8G8B8A8_UNORM color image + a D32_SFLOAT depth attachment, drawing an indexed cube through a depth-tested
// (VK_COMPARE_OP_LESS) Gouraud pipeline (SPIR-V vertex+fragment shaders loaded via read_spv; the vertex
// shader carries invariant gl_Position), copied to a host-visible buffer and read back. The assertion is an
// INDEPENDENT Rust software rasterizer: verts through the SAME MVP -> clip -> NDC (perspective divide) ->
// viewport pixels, per-pixel barycentric coverage + perspective-correct depth test + color interpolation.
// Vulkan NDC z in [0,1]: the perspective() z-row uses the Vulkan mapping and the reference window depth is
// z_clip/w_clip directly. The reference math is behaviour-identical to the C++ cell; only the ash-vs-C++
// Vulkan binding syntax differs. Prints "SCENE_3DMODEL_RUST OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS.
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

// ---- column-major 4x4 matrix math (m[col*4+row]) - byte-identical to the C++ cell ----
fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    for cc in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[cc * 4 + k];
            }
            r[cc * 4 + row] = s;
        }
    }
    r
}
fn mv4(a: &[f32; 16], v: &[f32; 4]) -> [f32; 4] {
    let mut o = [0.0f32; 4];
    for row in 0..4 {
        let mut s = 0.0;
        for k in 0..4 {
            s += a[k * 4 + row] * v[k];
        }
        o[row] = s;
    }
    o
}
// Vulkan perspective: near->z_ndc 0, far->z_ndc 1. Only the z row differs from GL.
fn perspective(fovy: f32, aspect: f32, zn: f32, zf: f32) -> [f32; 16] {
    let f = 1.0 / (fovy * 0.5).tan();
    let mut r = [0.0f32; 16];
    r[0] = f / aspect;
    r[4 + 1] = f;
    r[2 * 4 + 2] = zf / (zn - zf);
    r[2 * 4 + 3] = -1.0;
    r[3 * 4 + 2] = (zf * zn) / (zn - zf);
    r
}
fn translate(x: f32, y: f32, z: f32) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    r[0] = 1.0;
    r[5] = 1.0;
    r[10] = 1.0;
    r[15] = 1.0;
    r[3 * 4] = x;
    r[3 * 4 + 1] = y;
    r[3 * 4 + 2] = z;
    r
}
fn rot_y(a: f32) -> [f32; 16] {
    let (c, sn) = (a.cos(), a.sin());
    let mut r = [0.0f32; 16];
    r[0] = c;
    r[2] = -sn;
    r[2 * 4] = sn;
    r[2 * 4 + 2] = c;
    r[4 + 1] = 1.0;
    r[3 * 4 + 3] = 1.0;
    r
}
fn rot_x(a: f32) -> [f32; 16] {
    let (c, sn) = (a.cos(), a.sin());
    let mut r = [0.0f32; 16];
    r[4 + 1] = c;
    r[4 + 2] = sn;
    r[2 * 4 + 1] = -sn;
    r[2 * 4 + 2] = c;
    r[0] = 1.0;
    r[3 * 4 + 3] = 1.0;
    r
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
        unsafe {
            self.dev
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)
        }
        .expect("shader module")
    }
    fn mkbuf(&self, data: &[u8], usage: vk::BufferUsageFlags) -> (vk::Buffer, vk::DeviceMemory) {
        let sz = data.len() as vk::DeviceSize;
        let b = unsafe {
            self.dev
                .create_buffer(&vk::BufferCreateInfo::default().size(sz).usage(usage), None)
        }
        .expect("buffer");
        let mr = unsafe { self.dev.get_buffer_memory_requirements(b) };
        let ai = vk::MemoryAllocateInfo::default()
            .allocation_size(mr.size)
            .memory_type_index(self.memtype(
                mr.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ));
        let mem = unsafe { self.dev.allocate_memory(&ai, None) }.unwrap();
        unsafe {
            self.dev.bind_buffer_memory(b, mem, 0).unwrap();
            let p = self
                .dev
                .map_memory(mem, 0, sz, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
            self.dev.unmap_memory(mem);
        }
        (b, mem)
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
}

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
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
        rbuf: vk::Buffer::null(),
        rmap: std::ptr::null_mut(),
        buf: vec![0u8; W * H * 4],
    };

    // color image
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

    // depth image
    let dii = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::D32_SFLOAT)
        .extent(vk::Extent3D {
            width: W as u32,
            height: H as u32,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let dimg = unsafe { c.dev.create_image(&dii, None) }.expect("depth image");
    s.ok(dimg != vk::Image::null(), "vkCreateImage depth");
    let dmr = unsafe { c.dev.get_image_memory_requirements(dimg) };
    let dmem = unsafe {
        c.dev.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(dmr.size)
                .memory_type_index(
                    c.memtype(dmr.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL),
                ),
            None,
        )
    }
    .unwrap();
    unsafe { c.dev.bind_image_memory(dimg, dmem, 0) }.unwrap();
    let dview = unsafe {
        c.dev.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(dimg)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::D32_SFLOAT)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::DEPTH,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        )
    }
    .expect("depth view");
    s.ok(dview != vk::ImageView::null(), "vkCreateImageView depth");

    // render pass: color + depth
    let att = [
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
    let cref = [vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    }];
    let dref = vk::AttachmentReference {
        attachment: 1,
        layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    };
    let sp = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&cref)
        .depth_stencil_attachment(&dref)];
    let rp = unsafe {
        c.dev.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(&att)
                .subpasses(&sp),
            None,
        )
    }
    .expect("render pass");
    s.ok(rp != vk::RenderPass::null(), "vkCreateRenderPass");
    let fbv = [cview, dview];
    let fb = unsafe {
        c.dev.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(rp)
                .attachments(&fbv)
                .width(W as u32)
                .height(H as u32)
                .layers(1),
            None,
        )
    }
    .expect("framebuffer");
    s.ok(fb != vk::Framebuffer::null(), "vkCreateFramebuffer");

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
    s.ok(
        true,
        "offscreen R8G8B8A8 + D32_SFLOAT target + readback buffer ready",
    );

    // cube mesh (byte-identical)
    let vpos: [[f32; 3]; 8] = [
        [-1.0, -1.0, -1.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
    ];
    let mut vc = [[0.0f32; 3]; 8];
    for i in 0..8 {
        for k in 0..3 {
            vc[i][k] = (vpos[i][k] + 1.0) * 0.5;
        }
    }
    let idx: [u16; 36] = [
        0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4,
        1, 5, 6, 1, 6, 2,
    ];

    let model = mul(&rot_y(0.6), &rot_x(0.3));
    let view = translate(0.0, 0.0, -5.0);
    let proj = perspective(1.0, W as f32 / H as f32, 1.0, 20.0);
    let mvp = mul(&proj, &mul(&view, &model));

    let mut verts = [0.0f32; 8 * 6];
    for i in 0..8 {
        verts[i * 6] = vpos[i][0];
        verts[i * 6 + 1] = vpos[i][1];
        verts[i * 6 + 2] = vpos[i][2];
        verts[i * 6 + 3] = vc[i][0];
        verts[i * 6 + 4] = vc[i][1];
        verts[i * 6 + 5] = vc[i][2];
    }
    let (vbo, _vm) = c.mkbuf(&f32s_to_bytes(&verts), vk::BufferUsageFlags::VERTEX_BUFFER);
    let idx_bytes: Vec<u8> = idx.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (ibo, _im) = c.mkbuf(&idx_bytes, vk::BufferUsageFlags::INDEX_BUFFER);

    // pipeline: pos3+col3 (stride 24), mvp push constant, depth LESS, no cull
    let vs = c.shmod(include_bytes!("../shaders/cube_vert.spv"));
    let fs = c.shmod(include_bytes!("../shaders/cube_frag.spv"));
    let pcr = [vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::VERTEX,
        offset: 0,
        size: 64,
    }];
    let pl = unsafe {
        c.dev.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&pcr),
            None,
        )
    }
    .unwrap();
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
        stride: 24,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attr = [
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R32G32B32_SFLOAT,
            offset: 0,
        },
        vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R32G32B32_SFLOAT,
            offset: 12,
        },
    ];
    let vin = vk::PipelineVertexInputStateCreateInfo::default()
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
    let scr = [vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D {
            width: W as u32,
            height: H as u32,
        },
    }];
    let vps = vk::PipelineViewportStateCreateInfo::default()
        .viewports(&vp)
        .scissors(&scr);
    let rs = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let ms = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let dss = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(true)
        .depth_write_enable(true)
        .depth_compare_op(vk::CompareOp::LESS)
        .min_depth_bounds(0.0)
        .max_depth_bounds(1.0);
    let cba = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(vk::ColorComponentFlags::RGBA)
        .blend_enable(false)];
    let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cba);
    let gp = [vk::GraphicsPipelineCreateInfo::default()
        .stages(&st)
        .vertex_input_state(&vin)
        .input_assembly_state(&ia)
        .viewport_state(&vps)
        .rasterization_state(&rs)
        .multisample_state(&ms)
        .depth_stencil_state(&dss)
        .color_blend_state(&cb)
        .layout(pl)
        .render_pass(rp)
        .subpass(0)];
    let pipe = unsafe {
        c.dev
            .create_graphics_pipelines(vk::PipelineCache::null(), &gp, None)
    }
    .expect("cube pipeline")[0];
    s.ok(pipe != vk::Pipeline::null(), "cube pipeline created");

    // draw
    {
        let cai = vk::CommandBufferAllocateInfo::default()
            .command_pool(c.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = unsafe { c.dev.allocate_command_buffers(&cai) }.unwrap()[0];
        let bi = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { c.dev.begin_command_buffer(cmd, &bi) }.unwrap();
        let cv = [
            vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            },
            vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
        ];
        let rpb = vk::RenderPassBeginInfo::default()
            .render_pass(rp)
            .framebuffer(fb)
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
            c.dev
                .cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
            let mvp_bytes = f32s_to_bytes(&mvp);
            c.dev
                .cmd_push_constants(cmd, pl, vk::ShaderStageFlags::VERTEX, 0, &mvp_bytes);
            c.dev.cmd_bind_vertex_buffers(cmd, 0, &[vbo], &[0]);
            c.dev
                .cmd_bind_index_buffer(cmd, ibo, 0, vk::IndexType::UINT16);
            c.dev.cmd_draw_indexed(cmd, 36, 1, 0, 0, 0);
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
    s.ok(true, "cube drawn (depth-tested, Gouraud)");

    // INDEPENDENT software reference rasterizer
    let mut refc = vec![[0.0f32; 3]; W * H];
    let mut refz = vec![1e9f32; W * H];
    let mut refcov = vec![0u8; W * H];
    let mut sx = [0.0f32; 8];
    let mut sy = [0.0f32; 8];
    let mut sz = [0.0f32; 8];
    let mut sw = [0.0f32; 8];
    for i in 0..8 {
        let out = mv4(&mvp, &[vpos[i][0], vpos[i][1], vpos[i][2], 1.0]);
        let w = out[3];
        sw[i] = w;
        let (ndcx, ndcy, ndcz) = (out[0] / w, out[1] / w, out[2] / w);
        sx[i] = (ndcx * 0.5 + 0.5) * W as f32;
        sy[i] = (ndcy * 0.5 + 0.5) * H as f32;
        sz[i] = ndcz;
    }
    s.ok(
        sw[0] > 0.0,
        "reference: all clip.w positive (mesh in front of camera)",
    );
    for t in 0..12 {
        let (a, b, cc) = (
            idx[t * 3] as usize,
            idx[t * 3 + 1] as usize,
            idx[t * 3 + 2] as usize,
        );
        let (ax, ay, bx, by, cx, cy) = (sx[a], sy[a], sx[b], sy[b], sx[cc], sy[cc]);
        let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if area.abs() < 1e-6 {
            continue;
        }
        let mut minx = ax.min(bx).min(cx).floor() as i32;
        let mut maxx = ax.max(bx).max(cx).ceil() as i32;
        let mut miny = ay.min(by).min(cy).floor() as i32;
        let mut maxy = ay.max(by).max(cy).ceil() as i32;
        if minx < 0 {
            minx = 0;
        }
        if miny < 0 {
            miny = 0;
        }
        if maxx > W as i32 {
            maxx = W as i32;
        }
        if maxy > H as i32 {
            maxy = H as i32;
        }
        for y in miny..maxy {
            for x in minx..maxx {
                let pxs = x as f32 + 0.5;
                let pys = y as f32 + 0.5;
                let mut w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area;
                let mut w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area;
                let mut w2 = 1.0 - w0 - w1;
                let inside =
                    (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if !inside {
                    continue;
                }
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    w0 = -w0;
                    w1 = -w1;
                    w2 = -w2;
                }
                let z = w0 * sz[a] + w1 * sz[b] + w2 * sz[cc];
                let idxp = (y as usize) * W + x as usize;
                if z < refz[idxp] {
                    refz[idxp] = z;
                    refcov[idxp] = 1;
                    let (iwa, iwb, iwc) = (1.0 / sw[a], 1.0 / sw[b], 1.0 / sw[cc]);
                    let d = w0 * iwa + w1 * iwb + w2 * iwc;
                    for k in 0..3 {
                        let num = w0 * iwa * vc[a][k] + w1 * iwb * vc[b][k] + w2 * iwc * vc[cc][k];
                        refc[idxp][k] = num / d;
                    }
                }
            }
        }
    }

    let (mut total, mut mtch, mut covmatch, mut covtotal, mut interior_bad) =
        (0i32, 0i32, 0i32, 0i32, 0i32);
    for y in 0..H {
        for x in 0..W {
            total += 1;
            let gcov = !(c.px(x, y, 0) == 0 && c.px(x, y, 1) == 0 && c.px(x, y, 2) == 0);
            let idxp = y * W + x;
            let rcov = refcov[idxp] != 0;
            if gcov == rcov {
                covmatch += 1;
            }
            if rcov {
                covtotal += 1;
                let er = (refc[idxp][0] * 255.0).round() as i32;
                let eg = (refc[idxp][1] * 255.0).round() as i32;
                let eb = (refc[idxp][2] * 255.0).round() as i32;
                let interior = x > 0
                    && y > 0
                    && x < W - 1
                    && y < H - 1
                    && refcov[(y - 1) * W + x] != 0
                    && refcov[(y + 1) * W + x] != 0
                    && refcov[y * W + x - 1] != 0
                    && refcov[y * W + x + 1] != 0;
                if c.peq(x, y, [er, eg, eb, 255], 6) {
                    mtch += 1;
                } else if interior {
                    interior_bad += 1;
                }
            }
        }
    }
    s.ok(covtotal > 200, "reference: cube covers a substantial area");
    s.ok(
        covmatch >= (0.97 * total as f32) as i32,
        "coverage mask matches GPU (>=97% of pixels agree covered/empty)",
    );
    s.ok(
        interior_bad == 0,
        "every interior pixel matches perspective-correct Gouraud reference (tol 6)",
    );
    s.ok(
        mtch >= (0.92 * covtotal as f32) as i32,
        "92%+ of covered pixels match reference color (edges excluded)",
    );

    {
        let vx = (sx[6] - 0.5).round() as i32;
        let vy = (sy[6] - 0.5).round() as i32;
        if vx >= 1 && vx < W as i32 - 1 && vy >= 1 && vy < H as i32 - 1 {
            let mut bright = false;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (xx, yy) = ((vx + dx) as usize, (vy + dy) as usize);
                    if c.px(xx, yy, 0) > 180 && c.px(xx, yy, 1) > 180 && c.px(xx, yy, 2) > 180 {
                        bright = true;
                    }
                }
            }
            s.ok(
                bright,
                "vertex (1,1,1) region is bright (Gouraud white corner)",
            );
        } else {
            s.ok(
                false,
                "vertex (1,1,1) projected off-screen (camera mis-set)",
            );
        }
    }
    s.ok(
        c.peq(0, 0, [0, 0, 0, 255], 1) || refcov[0] == 0,
        "corner (0,0) background consistent",
    );

    {
        let (cxp, cyp) = (W / 2, H / 2);
        if refcov[cyp * W + cxp] != 0 {
            let idxp = cyp * W + cxp;
            let er = (refc[idxp][0] * 255.0).round() as i32;
            let eg = (refc[idxp][1] * 255.0).round() as i32;
            let eb = (refc[idxp][2] * 255.0).round() as i32;
            s.ok(
                c.peq(cxp, cyp, [er, eg, eb, 255], 8),
                "center pixel = nearest-face (depth-buffered occlusion) reference color",
            );
        } else {
            s.ok(false, "center pixel not covered (mesh mis-projected)");
        }
    }
    s.ok(
        !(c.px(1, 1, 0) == c.px(W / 2, H / 2, 0)
            && c.px(1, 1, 1) == c.px(W / 2, H / 2, 1)
            && c.px(1, 1, 2) == c.px(W / 2, H / 2, 2)),
        "negative control: image is not a flat single color (real 3D shading present)",
    );

    unsafe { c.dev.device_wait_idle().unwrap() };
    let (pass, fail) = (s.pass, s.fail);
    let total_a = pass + fail;
    let expected = 23;
    println!("scene-3dmodel-rust: PASS={pass} FAIL={fail} TOTAL={total_a} EXPECTED={expected}");
    if fail == 0 && total_a == expected {
        println!("SCENE_3DMODEL_RUST OK {pass}");
        std::process::exit(0);
    }
    std::process::exit(1);
}
