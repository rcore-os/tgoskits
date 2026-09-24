//! Linux VirtIO GPU driver ioctl translation for `/dev/dri/card0`.
//!
//! GEM, PRIME, modeset and per-file resource state remain owned by `Card0`.

use super::*;

fn with_virgl<T>(
    operation: impl FnOnce(&mut dyn rdif_gpu::VirglOps) -> Result<T, rdif_gpu::GpuError>,
) -> VfsResult<T> {
    ax_gpu::with_gpu(|device| {
        device
            .virgl()
            .ok_or(rdif_gpu::GpuError::Unsupported)
            .and_then(operation)
    })
    .map_err(map_gpu_err)?
    .map_err(map_gpu_err)
}

fn virgl_capabilities() -> VfsResult<rdif_gpu::GpuCapabilities> {
    let identity = ax_gpu::identity().ok_or(VfsError::NotFound)?;
    if identity.driver_name != "virtio_gpu" {
        return Err(VfsError::NotATty);
    }
    ax_gpu::capabilities().ok_or(VfsError::NotFound)
}

fn wait_completion(completion: rdif_gpu::Completion) -> VfsResult<()> {
    loop {
        let status = ax_gpu::with_gpu(|device| device.completion_status(completion))
            .map_err(map_gpu_err)?
            .map_err(map_gpu_err)?;
        if status == rdif_gpu::CompletionStatus::Complete {
            return Ok(());
        }
        crate::task::yield_now();
    }
}

impl Card0 {
    // ======== virtgpu ioctl handlers ========
    //
    // These implement Linux's 11 VirtIO GPU driver ioctls used by Mesa.
    // Generic GEM, PRIME and KMS operations do not enter this module.
    //
    // Security: Each handler validates input from userspace before use,
    // matching Linux kernel behavior (bounds checks, EINVAL for invalid
    // params, EEXIST for duplicate context init, etc.).

    /// VIRTGPU_GETPARAM — queries driver parameters.
    ///
    /// Linux: `virtgpu_getparam_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Mesa queries all parameters during initialization. Known parameters
    /// return their values; unknown parameters return `-EINVAL` (matching
    /// Linux kernel behavior — Mesa handles this gracefully).
    pub(super) fn handle_virtgpu_getparam(&self, current: &UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuGetparam;
        let g: DrmVirtgpuGetparam = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        let capabilities = virgl_capabilities()?;
        let has_virgl = capabilities.supports_3d;

        let value = match g.param {
            VIRTGPU_PARAM_3D_FEATURES => {
                // Must return 1 for Mesa to use virgl path.
                if has_virgl { 1 } else { 0 }
            }
            VIRTGPU_PARAM_CAPSET_QUERY_FIX => {
                // Linux 内核总是返回 1，不管 has_virgl_3d。
                // 这影响 GET_CAPS 的行为（Mesa 用它决定查询顺序）。
                1
            }
            VIRTGPU_PARAM_RESOURCE_BLOB => {
                // Report the *actual* negotiated feature (Linux:
                // `has_resource_blob ? 1 : 0`). Without RESOURCE_BLOB the
                // device doesn't support blobs and Mesa must use the classic
                // resource path — reporting 1 here would make Mesa create
                // blobs that fail.
                if capabilities.supports_blob {
                    1
                } else {
                    0
                }
            }
            VIRTGPU_PARAM_HOST_VISIBLE => {
                // RESOURCE_BLOB alone does not make host memory mappable.
                // Until RESOURCE_MAP_BLOB/BAR mapping exists, advertising
                // HOST_VISIBLE would send Mesa down a path we cannot honor.
                0
            }
            VIRTGPU_PARAM_CROSS_DEVICE => {
                // Cross-device sharing not supported yet.
                0
            }
            VIRTGPU_PARAM_CONTEXT_INIT => {
                // Report the *actual* negotiated feature (Linux:
                // `has_context_init ? 1 : 0`). Must be 1 for Mesa to use the
                // context-init protocol, but VIRGL alone does not imply it: a
                // legacy device can support virgl without CONTEXT_INIT.
                if capabilities.supports_context_init { 1 } else { 0 }
            }
            VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS => {
                let mut mask = 0u64;
                if has_virgl {
                    for index in 0..MAX_CAPSET_ENUM {
                        let Ok(info) = with_virgl(|virgl| virgl.capset_info(index)) else {
                            break;
                        };
                        if info.id < u64::BITS {
                            mask |= 1u64 << info.id;
                        }
                    }
                }
                mask
            }
            _ => {
                // Unknown parameter — match Linux kernel: return -EINVAL.
                // Mesa handles this gracefully (value stays 0).
                return Err(VfsError::InvalidInput);
            }
        };

        // Linux `virtio_gpu_getparam_ioctl`: `copy_to_user((void __user *)
        // param->value, &value, sizeof(value))` — 结果写入用户指针指向的 u64,
        // 而不是写回 struct 字段。之前 `g.value = value; vm_write(g)` 把值写进
        // struct 的 value 字段(覆盖了指针),mesa 读的是指针指向的本地变量,
        // 导致所有 GETPARAM 都读到 0 → 3D_FEATURES=0 → virgl winsys 创建失败。
        if g.value == 0 {
            return Err(VfsError::BadAddress);
        }
        vm_write_slice(current, g.value as *mut u64, &[value]).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// VIRTGPU_CONTEXT_INIT — initializes a rendering context on this fd.
    ///
    /// Linux: `virtgpu_context_init_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// **Critical**: This is a pure-input ioctl with no output fields.
    /// The context is implicitly bound to the file descriptor. Each fd
    /// can only call CONTEXT_INIT once (repeated calls return -EEXIST).
    ///
    /// Mesa calls this with num_params=1 and a single parameter:
    ///   { param=VIRTGPU_CONTEXT_PARAM_CAPSET_ID, value=VIRGL2(2) or VIRGL1(1) }
    pub(super) fn handle_virtgpu_context_init(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let init: DrmVirtgpuContextInit = (arg as *const DrmVirtgpuContextInit)
            .vm_read(current)
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: if (!vgdev->has_context_init || !vgdev->has_virgl_3d)
        //           return -EINVAL;
        let capabilities = virgl_capabilities()?;
        if !capabilities.supports_context_init || !capabilities.supports_3d {
            return Err(VfsError::InvalidInput);
        }

        // Linux kernel: each fd can only call CONTEXT_INIT once.
        if file.context.lock().is_some() {
            return Err(VfsError::AlreadyExists);
        }

        // StarryOS currently implements the three context parameters below.
        if init.num_params > 3 {
            return Err(VfsError::InvalidInput);
        }
        if init.num_params > 0 && init.ctx_set_params == 0 {
            return Err(VfsError::BadAddress);
        }

        // Read the parameter array from userspace.
        let mut capset_id: u32 = 0;
        let mut num_rings: u32 = 1; // Linux default

        if init.num_params > 0 {
            let params_ptr = init.ctx_set_params as *const DrmVirtgpuContextSetParam;
            for i in 0..init.num_params as usize {
                let param: DrmVirtgpuContextSetParam = unsafe { params_ptr.add(i) }
                    .vm_read(current)
                    .map_err(|_| VfsError::BadAddress)?;
                match param.param {
                    VIRTGPU_CONTEXT_PARAM_CAPSET_ID => {
                        capset_id = param.value as u32;
                        // Linux: if (value > MAX_CAPSET_ID) return -EINVAL;
                        // MAX_CAPSET_ID in Linux v6.1 is 6 (VIRTGPU_DRM_CAPSET_DRM)
                        if capset_id > VIRTGPU_DRM_CAPSET_DRM {
                            return Err(VfsError::InvalidInput);
                        }
                        // Linux: if ((vgdev->capset_id_mask & (1ULL << value)) == 0)
                        //           return -EINVAL;
                        // 我们支持 VIRGL(1) 和 VIRGL2(2)
                        if capset_id != VIRTGPU_DRM_CAPSET_VIRGL
                            && capset_id != VIRTGPU_DRM_CAPSET_VIRGL2
                        {
                            warn!("[card0] CONTEXT_INIT: unsupported capset_id={capset_id}");
                            return Err(VfsError::InvalidInput);
                        }
                    }
                    VIRTGPU_CONTEXT_PARAM_NUM_RINGS => {
                        num_rings = param.value as u32;
                        // Sanity check: limit rings.
                        if num_rings == 0 || num_rings > 64 {
                            return Err(VfsError::InvalidInput);
                        }
                    }
                    VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK => {
                        // Accept but ignore — we don't support polling yet.
                        let _ = param.value;
                    }
                    _ => {
                        // Unknown parameter — Linux returns -EINVAL.
                        return Err(VfsError::InvalidInput);
                    }
                }
            }
        }

        // Context creation itself is shared with the legacy lazy paths; the
        // EEXIST check above already covers a context an earlier lazy path
        // published on this fd.
        let ctx_id = file.create_context(CreateKind::Explicit {
            capset_id,
            num_rings,
        })?;

        info!(
            "[card0] CONTEXT_INIT: ctx_id={ctx_id:?}, capset_id={capset_id}, num_rings={num_rings}"
        );
        Ok(0)
    }

    /// VIRTGPU_GET_CAPS — retrieves capability set data.
    ///
    /// Linux: `virtgpu_get_caps_ioctl()` in `virtgpu_ioctl.c`.
    ///
    /// Semantics mirrored from Linux v7.1: the requested capset must exist in
    /// the device's real capset list with a `max_version` at least the
    /// requested version, a zero `size` is rejected, the copy uses
    /// `min(size, host_caps_size)`, and the input struct is never written
    /// back. Mesa first tries cap_set_id=2 (VIRGL2), then falls back to 1
    /// (VIRGL).
    pub(super) fn handle_virtgpu_get_caps(&self, current: &UserTaskRef, arg: usize) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuGetCaps;
        let g: DrmVirtgpuGetCaps = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        // Linux never writes the request struct back, so the caller's `size`
        // is a pure input; keep it in a local instead of mutating `g`.
        let user_size = g.size;

        // `num_capsets == 0` is reported as -ENOSYS by Linux.
        if !virgl_capabilities()?.supports_3d {
            return Err(VfsError::Unsupported);
        }

        // `gpu3d_capset_info` exposes GET_CAPSET_INFO by index, which is how
        // Linux enumerates `vgdev->capsets[]`. Index 0 succeeding proves the
        // device has at least one capset; the first failing index ends the
        // list, and the bound keeps a misbehaving host from looping forever.
        let mut capsets = Vec::new();
        for index in 0..MAX_CAPSET_ENUM {
            match with_virgl(|virgl| virgl.capset_info(index)) {
                Ok(info) => capsets.push(info),
                Err(_) => break,
            }
        }
        if capsets.is_empty() {
            return Err(VfsError::Unsupported);
        }

        // Linux: don't allow userspace to pass 0.
        if user_size == 0 {
            return Err(VfsError::InvalidInput);
        }

        // Select by ID equality and `max_version >= requested version`,
        // exactly as Linux scans the device's capset list. A missing or
        // too-old capset is -EINVAL, not a fabricated success.
        let matched = capsets
            .iter()
            .find(|info| info.id == g.cap_set_id && info.max_version >= g.cap_set_ver)
            .copied()
            .ok_or(VfsError::InvalidInput)?;

        let cache_key = (g.cap_set_id, g.cap_set_ver);
        let cached = self.capset_cache.lock().get(&cache_key).cloned();
        let cap_data = if let Some(data) = cached {
            data
        } else {
            // Ask the host for the full capset (`max_size`, not the user's
            // smaller `size`) so the cache entry is never truncated; only the
            // later copy is clamped. Truncating the query would hand Mesa an
            // incomplete capset and make it enable unsupported GL features.
            let data = with_virgl(|virgl| virgl.capset(g.cap_set_id, g.cap_set_ver, matched.max_size))?;
            self.capset_cache.lock().insert(cache_key, data.clone());
            data
        };

        // Linux copies `min(args->size, host_caps_size)` bytes; `cap_data` is
        // that host-side blob.
        let write_size = (user_size as usize).min(cap_data.len());
        if write_size == 0 || g.addr == 0 {
            return Err(VfsError::BadAddress);
        }
        vm_write_slice(current, g.addr as *mut u8, &cap_data[..write_size])
            .map_err(|_| VfsError::BadAddress)?;

        Ok(0)
    }

    /// VIRTGPU_RESOURCE_CREATE — creates a 3D resource.
    ///
    /// Linux: `virtgpu_resource_create_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Creates a 3D resource on the host and optionally associates it with
    /// an existing GEM handle. Returns the virtio-gpu resource ID in
    /// `res_handle` (NOT the GEM handle — they are different!).
    pub(super) fn handle_virtgpu_resource_create(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuResourceCreate;
        let mut r: DrmVirtgpuResourceCreate =
            ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        if !virgl_capabilities()?.supports_3d {
            return Err(VfsError::Unsupported);
        }
        // Linux `virtio_gpu_resource_create_ioctl()` calls
        // `virtio_gpu_create_context()` up front on the virgl path.
        file.ensure_lazy_context()?;

        let size = if r.size > 0 {
            r.size as u64
        } else {
            PAGE_SIZE_4K as u64
        };
        if size > DUMB_BUFFER_MAX_SIZE as u64 {
            return Err(VfsError::InvalidInput);
        }
        let alloc_size = (size as usize)
            .div_ceil(PAGE_SIZE_4K)
            .checked_mul(PAGE_SIZE_4K)
            .ok_or(VfsError::InvalidInput)?;
        let mapping = ax_gpu::allocate_mappable_backing(
            NonZeroUsize::new(alloc_size).ok_or(VfsError::InvalidInput)?,
        )
        .map_err(map_gpu_err)?;
        let mapping = Arc::new(GpuMapping::Owned(mapping));
        let bo_handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        let backing = mapping.backing();

        let device_handle = with_virgl(|virgl| virgl.create_resource_3d(rdif_gpu::Resource3d {
            target: r.target,
            format: r.format,
            bind: r.bind,
            width: r.width,
            height: r.height,
            depth: r.depth,
            array_size: r.array_size,
            last_level: r.last_level,
            samples: r.nr_samples,
            flags: r.flags,
        }, Some(backing)))?;
        let res_handle = match with_virgl(|virgl| virgl.command_resource_id(device_handle)) {
            Ok(id) => id,
            Err(error) => {
                let _ = ax_gpu::with_gpu(|device| device.release_buffer(device_handle));
                return Err(error);
            }
        };
        let resource = Arc::new(GpuResource {
            owner: file.file_id,
            res_handle,
            device_handle,
            bo_handle,
            width: r.width,
            height: r.height,
            stride: r.stride,
            format: None,
            size,
            blob_mem: 0,
            blob_flags: 0,
            is_dumb_2d: false,
            last_fence: AtomicU64::new(0),
        });
        file.attach_resource(&resource)?;

        let buffer = DumbBuffer {
            owner: file.file_id,
            width: r.width,
            height: r.height,
            bpp: 32,
            pitch: r.stride,
            size,
            offset,
            mapping,
            mappable: true,
            resource: Some(resource.clone()),
        };
        r.bo_handle = bo_handle;
        r.res_handle = res_handle;
        r.size = size as u32;
        if ptr.vm_write(current, r).is_err() {
            file.detach_resource(res_handle);
            return Err(VfsError::BadAddress);
        }

        self.dumbs.lock().insert(bo_handle, buffer);
        self.gpu_resources.lock().insert(res_handle, resource);

        Ok(0)
    }

    /// VIRTGPU_RESOURCE_INFO — queries resource information.
    ///
    /// Linux: `virtgpu_resource_info_ioctl()` in `virtgpu_ioctl.c`
    pub(super) fn handle_virtgpu_resource_info(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuResourceInfo;
        let mut info: DrmVirtgpuResourceInfo =
            ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        let resource = self
            .resource_for_handle(file, info.bo_handle)
            .ok_or(VfsError::NotFound)?;
        info.res_handle = resource.res_handle;
        info.size = resource.size as u32;
        info.blob_mem = resource.blob_mem;
        ptr.vm_write(current, info)
            .map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// VIRTGPU_MAP — maps a GEM handle to an mmap offset.
    ///
    /// Linux: `virtgpu_map_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Returns an offset that can be used with the mmap system call.
    /// This reuses the same offset mechanism as MAP_DUMB.
    pub(super) fn handle_virtgpu_map(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuMap;
        let mut m: DrmVirtgpuMap = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        let dumbs = self.dumbs.lock();
        let buf = dumbs
            .get(&m.handle)
            .filter(|buffer| buffer.owner == file.file_id && buffer.mappable)
            .ok_or(VfsError::InvalidInput)?;
        m.offset = buf.offset;
        drop(dumbs);

        ptr.vm_write(current, m).map_err(|_| VfsError::BadAddress)?;
        Ok(0)
    }

    /// VIRTGPU_EXECBUFFER — submits a virgl command buffer.
    ///
    /// Linux: `virtgpu_execbuffer_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// This is the core ioctl: Mesa submits VIRGL_CCMD_* command streams
    /// through this. The command buffer is read from userspace, along with
    /// an array of GEM handles that the commands reference.
    ///
    /// **Critical**: There is NO ctx_id field. The context is implicitly
    /// bound to the file descriptor.
    ///
    /// Returns `StarryResult` rather than `VfsResult`: the `FENCE_FD_OUT`
    /// descriptor pre-reservation can fail with `EMFILE`, which the `VfsError`
    /// domain cannot represent.
    pub(super) fn handle_virtgpu_execbuffer(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> StarryResult<usize> {
        let ptr = arg as *mut DrmVirtgpuExecbuffer;
        let mut eb: DrmVirtgpuExecbuffer =
            ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        // Linux: if (vgdev->has_virgl_3d == false) return -ENOSYS;
        if !virgl_capabilities()?.supports_3d {
            return Err(StarryError::Unsupported);
        }

        // Only the fence-fd in/out flags are supported. `VIRTGPU_EXECBUF_RING_IDX`
        // (0x04), the syncobj fields and any unknown flag stay unsupported and
        // must fail instead of being silently ignored (Linux checks
        // `exbuf->flags & ~VIRTGPU_EXECBUF_FLAGS`).
        let known_fence_flags = VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_FENCE_FD_OUT;
        if eb.flags & !known_fence_flags != 0
            || eb.ring_idx != 0
            || eb.syncobj_stride != 0
            || eb.num_in_syncobjs != 0
            || eb.num_out_syncobjs != 0
            || eb.in_syncobjs != 0
            || eb.out_syncobjs != 0
        {
            return Err(StarryError::InvalidInput);
        }
        let fence_in = eb.flags & VIRTGPU_EXECBUF_FENCE_FD_IN != 0;
        let fence_out = eb.flags & VIRTGPU_EXECBUF_FENCE_FD_OUT != 0;

        // Cheap input validation before any host side effect: a malformed
        // request must not create a context, attach a resource or submit work.
        if eb.size == 0
            || !eb.size.is_multiple_of(4)
            || eb.size as usize > MAX_VIRGL_COMMAND_BYTES
            || eb.command == 0
        {
            return Err(StarryError::InvalidInput);
        }
        if eb.num_bo_handles > 256 || (eb.num_bo_handles > 0 && eb.bo_handles == 0) {
            return Err(StarryError::InvalidInput);
        }

        // `FENCE_FD_IN` imports an existing fence fd. Only a sync_file created
        // by this driver carries one; a foreign object under that fd number is
        // -EINVAL, matching Linux `sync_file_get_fence()` returning NULL.
        if fence_in {
            if eb.fence_fd < 0 {
                return Err(StarryError::InvalidInput);
            }
            let file =
                crate::file::get_file_like(eb.fence_fd).map_err(|_| VfsError::InvalidInput)?;
            let fence = file
                .downcast_arc::<SyncFile>()
                .map_err(|_| VfsError::InvalidInput)?;
            // Every out-fence produced by this driver is already signaled by
            // the time its fd is visible, so the dependency is satisfied. An
            // unsignaled fence cannot be produced here; treat it as an invalid
            // import rather than parking the submit.
            if !fence.is_signaled() {
                return Err(StarryError::InvalidInput);
            }
        }

        // `FENCE_FD_OUT` reserves the descriptor *before* any side effect: an
        // fd shortage must fail the ioctl (with EMFILE) without having created
        // a context or queued GPU work. The reservation is released
        // automatically if a later step fails.
        let out_fence = if fence_out {
            let sync_file = Arc::new(SyncFile::new());
            let created: Arc<dyn FileLike> = sync_file.clone();
            let prepared = prepare_file_like(move || Ok(created), true)?;
            Some((prepared, sync_file))
        } else {
            None
        };

        // Read the command buffer and BO handles, then resolve every handle to
        // an owned resource — all before creating a context, so a bad command
        // buffer or GEM handle leaves no host state behind.
        let cmd_buf = vm_load(current, eb.command as *const u8, eb.size as usize)
            .map_err(|_| VfsError::BadAddress)?;
        let handles = if eb.num_bo_handles == 0 {
            Vec::new()
        } else {
            vm_load(
                current,
                eb.bo_handles as *const u32,
                eb.num_bo_handles as usize,
            )
            .map_err(|_| VfsError::BadAddress)?
        };
        let mut resources = Vec::with_capacity(handles.len());
        for handle in handles {
            let resource = self
                .resource_for_handle(file, handle)
                .ok_or(VfsError::NotFound)?;
            resources.push(resource);
        }

        // Context creation and resource attach are the first host side
        // effects; Linux reaches `virtio_gpu_create_context()` at the same
        // point, after the inputs above validated.
        let ctx_id = file.ensure_lazy_context()?;
        for resource in &resources {
            file.attach_resource(resource)?;
        }

        let completion = with_virgl(|virgl| virgl.submit(ctx_id, &cmd_buf))?;
        wait_completion(completion)?;
        for resource in resources {
            resource.last_fence.store(0, Ordering::Release);
        }

        // The submit completed synchronously: its host fence response was
        // already consumed, so an out-fence is signaled here and the fd only
        // becomes visible after this point. `fence_fd` is written back only
        // for `FENCE_FD_OUT`; an IN-only request keeps its input fd.
        if let Some((prepared, sync_file)) = out_fence {
            sync_file.mark_signaled();
            eb.fence_fd = prepared.fd();
            ptr.vm_write(current, eb)
                .map_err(|_| VfsError::BadAddress)?;
            prepared.install();
        } else {
            ptr.vm_write(current, eb)
                .map_err(|_| VfsError::BadAddress)?;
        }

        Ok(0)
    }

    /// VIRTGPU_TRANSFER_TO_HOST — transfers data from guest to host.
    ///
    /// Linux: `virtgpu_transfer_from_host_ioctl()` in `virtgpu_ioctl.c`
    /// (Note: Linux naming is confusing — "from_host" means "from guest
    /// memory to host" in the virtio-gpu spec.)
    pub(super) fn handle_virtgpu_transfer_to_host(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let t: DrmVirtgpu3dTransferToHost = (arg as *const DrmVirtgpu3dTransferToHost)
            .vm_read(current)
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: if (vgdev->has_virgl_3d == false) return -ENOSYS;
        if !virgl_capabilities()?.supports_3d {
            return Err(VfsError::Unsupported);
        }

        // Linux `virtio_gpu_transfer_to_host_ioctl()` creates the context on
        // the virgl branch before forwarding the 3D transfer.
        let ctx_id = file.ensure_lazy_context()?;
        let resource = self
            .resource_for_handle(file, t.bo_handle)
            .ok_or(VfsError::NotFound)?;
        file.attach_resource(&resource)?;

        let completion = with_virgl(|virgl| virgl.transfer_to_host(rdif_gpu::Transfer3d {
            context: ctx_id,
            resource: resource.device_handle,
            box_: rdif_gpu::TransferBox {
                x: t.box_.x,
                y: t.box_.y,
                z: t.box_.z,
                width: t.box_.w,
                height: t.box_.h,
                depth: t.box_.d,
            },
            offset: t.offset as u64,
            level: t.level,
            stride: t.stride,
            layer_stride: t.layer_stride,
        }))?;
        wait_completion(completion)?;

        Ok(0)
    }

    /// VIRTGPU_TRANSFER_FROM_HOST — transfers data from host to guest.
    ///
    /// Linux: `virtgpu_transfer_to_host_ioctl()` in `virtgpu_ioctl.c`
    pub(super) fn handle_virtgpu_transfer_from_host(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let t: DrmVirtgpu3dTransferFromHost = (arg as *const DrmVirtgpu3dTransferFromHost)
            .vm_read(current)
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: if (vgdev->has_virgl_3d == false) return -ENOSYS;
        if !virgl_capabilities()?.supports_3d {
            return Err(VfsError::Unsupported);
        }

        // Linux `virtio_gpu_transfer_from_host_ioctl()` likewise creates the
        // context before forwarding the 3D transfer.
        let ctx_id = file.ensure_lazy_context()?;
        let resource = self
            .resource_for_handle(file, t.bo_handle)
            .ok_or(VfsError::NotFound)?;
        file.attach_resource(&resource)?;

        let completion = with_virgl(|virgl| virgl.transfer_from_host(rdif_gpu::Transfer3d {
            context: ctx_id,
            resource: resource.device_handle,
            box_: rdif_gpu::TransferBox {
                x: t.box_.x,
                y: t.box_.y,
                z: t.box_.z,
                width: t.box_.w,
                height: t.box_.h,
                depth: t.box_.d,
            },
            offset: t.offset as u64,
            level: t.level,
            stride: t.stride,
            layer_stride: t.layer_stride,
        }))?;
        wait_completion(completion)?;

        Ok(0)
    }

    /// VIRTGPU_WAIT — waits for a resource to become idle.
    ///
    /// Linux: `virtgpu_wait_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// We implement this as a synchronous wait (the resource is always
    /// "ready" since we process commands synchronously).
    pub(super) fn handle_virtgpu_wait(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let w: DrmVirtgpu3dWait = (arg as *const DrmVirtgpu3dWait)
            .vm_read(current)
            .map_err(|_| VfsError::BadAddress)?;

        // Linux: handle=0 is invalid.
        if w.handle == 0 {
            return Err(VfsError::InvalidInput);
        }

        let has_dumb = self
            .dumbs
            .lock()
            .get(&w.handle)
            .is_some_and(|buffer| buffer.owner == file.file_id);
        let resource = self.resource_for_handle(file, w.handle);
        if !has_dumb && resource.is_none() {
            return Err(VfsError::NotFound);
        }
        // Every driver submission waits for its virtqueue completion, so a
        // returned ioctl has already completed the last fence for this GEM.
        let _last_fence = resource.map(|resource| resource.last_fence.load(Ordering::Acquire));
        Ok(0)
    }

    /// VIRTGPU_RESOURCE_CREATE_BLOB — creates a blob resource.
    ///
    /// Linux: `virtgpu_resource_create_blob_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Guest-backed blob types carry a guest memory entry; host-only blobs
    /// stay unmappable until RESOURCE_MAP_BLOB is implemented.
    pub(super) fn handle_virtgpu_resource_create_blob(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let ptr = arg as *mut DrmVirtgpuResourceCreateBlob;
        let mut b: DrmVirtgpuResourceCreateBlob =
            ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;

        // Linux: if (!vgdev->has_resource_blob) return -EINVAL;
        let capabilities = virgl_capabilities()?;
        if !capabilities.supports_blob {
            return Err(VfsError::InvalidInput);
        }
        // Linux: if (rc_blob->blob_flags & ~VIRTGPU_BLOB_FLAG_USE_MASK)
        //           return -EINVAL;
        // Note: VIRTGPU_BLOB_FLAG_USE_* 是 0x0001, 0x0002, 0x0004
        // VIRTGPU_BLOB_FLAG_USE_MASK = 0x0007
        if (b.blob_flags & !0x0007) != 0 {
            return Err(VfsError::InvalidInput);
        }
        // Linux `verify_blob()` refuses a cross-device blob when the device has
        // no RESOURCE_ASSIGN_UUID support: `!vgdev->has_resource_assign_uuid`
        // returns -EINVAL. GETPARAM already reports
        // VIRTGPU_PARAM_CROSS_DEVICE = 0 for this card, so reject the flag here
        // rather than forwarding it to a device that cannot honour it.
        if (b.blob_flags & VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE) != 0 {
            return Err(VfsError::InvalidInput);
        }

        // Validate blob memory type and determine blob category.
        let (guest_blob, host3d_blob) = match b.blob_mem {
            VIRTGPU_BLOB_MEM_GUEST => (true, false),
            VIRTGPU_BLOB_MEM_HOST3D_GUEST => (true, true),
            VIRTGPU_BLOB_MEM_HOST3D => (false, true),
            _ => {
                // Linux: default: return -EINVAL;
                return Err(VfsError::InvalidInput);
            }
        };

        // Linux: if (*host3d_blob) {
        //           if (!vgdev->has_virgl_3d) return -EINVAL;
        //           if (rc_blob->cmd_size % 4 != 0) return -EINVAL;
        if host3d_blob {
            if !capabilities.supports_3d {
                return Err(VfsError::InvalidInput);
            }
            // cmd_size 必须 4 字节对齐
            if !b.cmd_size.is_multiple_of(4) {
                return Err(VfsError::InvalidInput);
            }
        } else {
            // Linux: if (rc_blob->blob_id != 0) return -EINVAL;
            //        if (rc_blob->cmd_size != 0) return -EINVAL;
            if b.blob_id != 0 || b.cmd_size != 0 {
                return Err(VfsError::InvalidInput);
            }
        }

        // Linux `virtio_gpu_resource_create_blob_ioctl()` calls
        // `virtio_gpu_create_context()` on the virgl path before touching the
        // command stream, so the lazy context is created even for a pure guest
        // blob even though that blob is not attached to it. Host3D variants
        // are context-owned, so they keep the resulting ctx_id; a guest-only
        // blob still sends ctx_id = 0.
        let context = if capabilities.supports_3d {
            let ctx_id = file.ensure_lazy_context()?;
            host3d_blob.then_some(ctx_id)
        } else {
            None
        };

        if b.cmd_size as usize > MAX_VIRGL_COMMAND_BYTES
            || (b.cmd_size > 0 && b.cmd == 0)
            || b.size == 0
            || b.size > DUMB_BUFFER_MAX_SIZE as u64
        {
            return Err(VfsError::InvalidInput);
        }

        // Linux copies the command stream before creating/publishing the GEM
        // object. A bad userspace pointer must not leave a host resource.
        let cmd_buf = if b.cmd_size == 0 {
            Vec::new()
        } else {
            vm_load(current, b.cmd as *const u8, b.cmd_size as usize)
                .map_err(|_| VfsError::BadAddress)?
        };

        // Allocate a guest shadow buffer so VIRTGPU_MAP/mmap on the blob's
        // GEM handle keeps working. For HOST3D the real backing lives on
        // the host and we deliberately do NOT send these pages to the
        // device (nr_entries=0): QEMU and virglrenderer reject HOST3D blobs
        // that carry an iov. The shadow is only for mmap compatibility —
        // the present path is zero-copy on the host, no CPU readback.
        let alloc_size = (b.size as usize).div_ceil(PAGE_SIZE_4K) * PAGE_SIZE_4K;
        let mapping = ax_gpu::allocate_mappable_backing(
            NonZeroUsize::new(alloc_size).ok_or(VfsError::InvalidInput)?,
        )
        .map_err(map_gpu_err)?;
        let mapping = Arc::new(GpuMapping::Owned(mapping));
        let backing: Option<Arc<dyn rdif_gpu::Backing>> = if guest_blob {
            Some(mapping.backing())
        } else {
            None
        };
        let bo_handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
        let offset = self
            .next_offset
            .fetch_add(DUMB_BUFFER_OFFSET_STRIDE, Ordering::Relaxed);
        let device_handle = with_virgl(|virgl| virgl.create_blob(context, rdif_gpu::BlobDescriptor {
            memory: b.blob_mem,
            flags: b.blob_flags,
            size: b.size,
            id: b.blob_id,
        }, backing, &cmd_buf))?;
        let res_handle = match with_virgl(|virgl| virgl.command_resource_id(device_handle)) {
            Ok(id) => id,
            Err(error) => {
                let _ = ax_gpu::with_gpu(|device| device.release_buffer(device_handle));
                return Err(error);
            }
        };
        let resource = Arc::new(GpuResource {
            owner: file.file_id,
            res_handle,
            device_handle,
            bo_handle,
            width: 0,
            height: 0,
            stride: 0,
            format: None,
            size: b.size,
            blob_mem: b.blob_mem,
            blob_flags: b.blob_flags,
            is_dumb_2d: false,
            last_fence: AtomicU64::new(0),
        });
        if host3d_blob {
            file.attach_resource(&resource)?;
        }
        let buffer = DumbBuffer {
            owner: file.file_id,
            width: 0,
            height: 0,
            bpp: 0,
            pitch: 0,
            size: b.size,
            offset,
            mapping,
            mappable: guest_blob,
            resource: Some(resource.clone()),
        };

        b.bo_handle = bo_handle;
        b.res_handle = res_handle;
        if ptr.vm_write(current, b).is_err() {
            if host3d_blob {
                file.detach_resource(res_handle);
            }
            return Err(VfsError::BadAddress);
        }

        self.dumbs.lock().insert(bo_handle, buffer);
        self.gpu_resources.lock().insert(res_handle, resource);

        Ok(0)
    }
}
