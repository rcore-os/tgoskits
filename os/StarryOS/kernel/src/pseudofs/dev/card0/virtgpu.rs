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

/// Bitmask of the capset ids the host reported, or `None` when the device
/// has no 3D face. `CONTEXT_INIT` accepts any listed capset, which is how
/// the venus face (capset 4) is enabled on hosts that expose it.
fn device_capset_mask() -> Option<u64> {
    let mut mask = 0u64;
    for index in 0..MAX_CAPSET_ENUM {
        let Ok(info) = with_virgl(|virgl| virgl.capset_info(index)) else {
            break;
        };
        if info.id < u64::BITS {
            mask |= 1u64 << info.id;
        }
    }
    (mask != 0).then_some(mask)
}

/// Upper bound for one venus EXECBUFFER command buffer (and the venus blob
/// init stream). Real venus payloads are orders of magnitude smaller; the
/// bound exists so an oversized user size is rejected with EINVAL *before*
/// `vm_load` allocates the whole buffer — the loader itself has no
/// per-request ceiling for fixed-size loads.
const MAX_VENUS_COMMAND_BYTES: usize = 128 * 1024;

fn virgl_capabilities() -> VfsResult<rdif_gpu::GpuCapabilities> {
    let identity = ax_gpu::identity().ok_or(VfsError::NotFound)?;
    if identity.driver_name != "virtio_gpu" {
        return Err(VfsError::NotATty);
    }
    ax_gpu::capabilities().ok_or(VfsError::NotFound)
}

/// The submit fence a completion token carries, or 0 when the driver
/// completed synchronously and there is nothing to observe. `last_fence = 0`
/// means "no outstanding fence" everywhere it is stored.
fn completion_fence(completion: rdif_gpu::Completion) -> u64 {
    match completion {
        rdif_gpu::Completion::Pending(fence) => fence.get(),
        rdif_gpu::Completion::Complete => 0,
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
    pub(super) fn handle_virtgpu_getparam(
        &self,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
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
                if capabilities.supports_blob { 1 } else { 0 }
            }
            VIRTGPU_PARAM_HOST_VISIBLE => {
                // Honest only when the bus exposed the hostmem window: mesa
                // venus allocates every host-visible buffer through blobs
                // mapped into it.
                u64::from(super::venus_available())
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
                if capabilities.supports_context_init {
                    1
                } else {
                    0
                }
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
            VIRTGPU_PARAM_BLOB_ALIGNMENT => {
                // Linux reports the negotiated alignment, or -ENOENT when
                // the feature was not negotiated (Mesa then assumes no
                // alignment requirement).
                match with_virgl(|virgl| Ok(virgl.blob_alignment()))? {
                    Some(alignment) => u64::from(alignment),
                    None => return Err(VfsError::NotFound),
                }
            }
            _ => {
                // Unknown parameter — match Linux kernel: return -EINVAL.
                // Mesa handles this gracefully (value stays 0).
                return Err(VfsError::InvalidInput);
            }
        };

        // Linux `virtio_gpu_getparam_ioctl`: `copy_to_user((void __user *)
        // param->value, &value, sizeof(value))` — the result goes to the
        // memory the `value` field points at, not back into the struct
        // field (overwriting it would clobber the pointer, and Mesa reads
        // the pointed-at local, so every GETPARAM used to return 0).
        // `value` is an `int` in the UAPI, so exactly four bytes are
        // written: a contract-faithful client passing `&int32` must not
        // have the adjacent four bytes stomped.
        if g.value == 0 {
            return Err(VfsError::BadAddress);
        }
        let narrow = value as u32;
        vm_write_slice(current, g.value as *mut u32, &[narrow])
            .map_err(|_| VfsError::BadAddress)?;
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
                        // The mask is the host's own capset list, so a venus
                        // (4) context is accepted exactly when the host
                        // exposes the venus capset.
                        let Some(mask) = device_capset_mask() else {
                            return Err(VfsError::InvalidInput);
                        };
                        if mask & (1u64 << capset_id) == 0 {
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
    pub(super) fn handle_virtgpu_get_caps(
        &self,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
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
            let data =
                with_virgl(|virgl| virgl.capset(g.cap_set_id, g.cap_set_ver, matched.max_size))?;
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
    /// Creates a 3D resource on the host and allocates a new GEM handle.
    /// Returns the virtio-gpu resource ID in
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

        let device_handle = with_virgl(|virgl| {
            virgl.create_resource_3d(
                rdif_gpu::Resource3d {
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
                },
                Some(backing),
            )
        })?;
        let res_handle = match with_virgl(|virgl| virgl.command_resource_id(device_handle)) {
            Ok(id) => id,
            Err(error) => {
                let _ = release_gpu_buffer_and_drain(device_handle);
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
            mapped_blob: Mutex::new(None),
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
        let dumb_offset = dumbs
            .get(&m.handle)
            .filter(|buffer| buffer.owner == file.file_id && buffer.mappable)
            .map(|buffer| buffer.offset);
        drop(dumbs);
        if let Some(offset) = dumb_offset {
            m.offset = offset;
            ptr.vm_write(current, m).map_err(|_| VfsError::BadAddress)?;
            return Ok(0);
        }
        // A host-visible venus blob maps through its hostmem mmap key, not
        // the dumb-offset space. The mapping lives on the shared resource,
        // so import clones resolve to the same window.
        let resource = self
            .resource_for_handle(file, m.handle)
            .ok_or(VfsError::InvalidInput)?;
        if resource.owner != file.file_id {
            return Err(VfsError::InvalidInput);
        }
        let map = (*resource.mapped_blob.lock()).ok_or(VfsError::InvalidInput)?;
        m.offset = map.mmap_key;

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
    /// The submit is fire-and-forget (Linux `virtio_gpu_cmd_submit`): the
    /// ioctl enqueues the batch, delivers it with one boundary notify and
    /// returns the fence id — it does not wait for the host. An out-fence
    /// (`FENCE_FD_OUT`) is a real fence: the sync_file starts unsignaled and
    /// flips when the host completed the batch.
    ///
    /// Returns `StarryResult` rather than `VfsResult`: the `FENCE_FD_OUT`
    /// descriptor creation can fail with `EMFILE`, which the `VfsError`
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

        // A venus (capset 4) context submits through the ring path with
        // timeline syncobjs; everything below the branch is the virgl
        // protocol, whose syncobj fields stay rejected.
        let ctx_capset = file.context.lock().as_ref().map(|ctx| ctx.capset_id);
        if ctx_capset == Some(VIRTGPU_DRM_CAPSET_VENUS) {
            return self.handle_venus_execbuffer(file, current, arg);
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
        // -EINVAL, matching Linux `sync_file_get_fence()` returning NULL. The
        // downcast is the validation — no wait follows, because mainline
        // `virtgpu_submit.c` `virtio_gpu_do_fence_wait()` skips the CPU wait
        // for an in-fence from the same fence context: the dependency is
        // already ordered by the control queue's strict FIFO (the batch that
        // produced the fence was enqueued before this one and the host
        // applies commands in queue order), and only a foreign context would
        // need `dma_fence_wait(in_fence, true)`. Every sync_file this kernel
        // exports is backed by this device's `submit_3d` fence, so every
        // import here is same-context.
        if fence_in {
            if eb.fence_fd < 0 {
                return Err(StarryError::InvalidInput);
            }
            let file =
                crate::file::get_file_like(eb.fence_fd).map_err(|_| VfsError::InvalidInput)?;
            file.downcast_arc::<SyncFile>()
                .map_err(|_| VfsError::InvalidInput)?;
        }

        // Linux reserves the out-fence fd (`get_unused_fd_flags`) before the
        // submit, so an fd shortage fails with EMFILE while the device has
        // seen nothing of this batch. The fence id only exists after the
        // submit, so the file is created as a placeholder here and bound to
        // the submit's fence below, before the fd is installed.
        let out_fd = if fence_out {
            let sync_file = Arc::new(SyncFile::new(0));
            let created: Arc<dyn FileLike> = sync_file.clone();
            let prepared = prepare_file_like(move || Ok(created), true)?;
            Some((sync_file, prepared))
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
        let fence_id = completion_fence(completion);
        if let Some((sync_file, _)) = &out_fd {
            sync_file.bind_fence_id(fence_id);
        }
        for resource in resources {
            resource.last_fence.store(fence_id, Ordering::Release);
        }

        // Linux: `VIRTGPU_EXECBUF_FENCE_FD_OUT` wraps the submit fence in a
        // sync_file and returns the fd (`virtgpu_execbuffer_ioctl`). The fence
        // starts unsignaled — the submit is fire-and-forget — and signals when
        // the host pops the fenced command, which the fence refresher and the
        // poll/wait refresh paths observe. A synchronous completion has no
        // fence to observe (`fence_id == 0`); its sync_file signals on the
        // first level check.
        if let Some((sync_file, _)) = &out_fd
            && fence_id != 0
        {
            sync_file.register();
        }
        // `fence_fd` is written back only for `FENCE_FD_OUT`; an IN-only
        // request keeps its input fd untouched (Linux only updates the field
        // when it created an out-fence).
        eb.fence_fd = writeback_fence_fd(eb.fence_fd, out_fd.as_ref().map(|(_, p)| p.fd()));
        ptr.vm_write(current, eb)
            .map_err(|_| VfsError::BadAddress)?;
        if let Some((_, prepared)) = out_fd {
            prepared.install();
        }

        // EXECBUFFER is a fire-and-forget transaction (optional ctx_attach +
        // submit); one boundary notify delivers it — Linux
        // `virtio_gpu_notify()` at the ioctl end.
        with_virgl(|virgl| {
            virgl.ctrl_notify();
            Ok(())
        })?;

        // The out-fence (when FENCE_FD_OUT) completes tens of µs from now;
        // kick the refresher into burst pumping so the fence signals within
        // one host round-trip instead of waiting for the next active tick.
        if fence_out && fence_id != 0 {
            kick_refresher();
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

        // Intentionally unobserved completion: TRANSFER_TO_HOST writes
        // guest data that was already in memory before the submit, so the
        // direction is fire-and-forget (Linux does not fence it either);
        // the completion token carries no release proof to observe.
        let _ = with_virgl(|virgl| {
            virgl.transfer_to_host(rdif_gpu::Transfer3d {
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
            })
        })?;
        // Fire-and-forget (Linux does not fence this direction): the transfer
        // is ordered behind everything already submitted and ahead of every
        // later fence; deliver it at the ioctl boundary.
        with_virgl(|virgl| {
            virgl.ctrl_notify();
            Ok(())
        })?;

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

        // The read-back completion proof sits outside the device layer now:
        // the transfer is submitted fire-and-forget with a fence like Linux
        // `virtio_gpu_transfer_from_host_ioctl()`, and the ioctl still
        // returns only after the host applied the data — but the fence wait
        // sleeps without the control lock, so the completion pump keeps
        // running while we block. Waiting the transfer's own fence (not a
        // whole-queue drain) also means concurrent producers cannot starve
        // the readback. Linux relies on dma_resv deferred destruction for
        // memory safety instead; our unref/backing release has no dma_resv
        // equivalent, so the fence observation remains the proof that the
        // host no longer touches the guest memory.
        let completion = with_virgl(|virgl| {
            virgl.transfer_from_host(rdif_gpu::Transfer3d {
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
            })
        })?;
        wait_completion_outside_lock(completion).map_err(map_gpu_err)?;
        // Only now — after the host write completed — make the bytes
        // CPU-visible. The sync used to run inside the device layer's inline
        // drain; running it before observing the fence would race the host
        // write it exists to order against. A host-side resource (a blob
        // created with guest_blob == false) has no CPU-visible bytes at
        // all: Linux `virtio_gpu_transfer_from_host_ioctl()` submits,
        // returns 0 and syncs nothing for it, and so do we.
        if let Some(backing) =
            ax_gpu::with_gpu(|device| device.buffer_backing(resource.device_handle))
                .and_then(core::convert::identity)
                .map_err(map_gpu_err)?
        {
            backing
                .sync_for_cpu(0..backing.len())
                .map_err(map_gpu_err)?;
        }

        Ok(0)
    }

    /// VIRTGPU_WAIT — waits for a resource to become idle.
    ///
    /// Linux: `virtgpu_wait_ioctl()` in `virtgpu_ioctl.c`
    ///
    /// Submits are fire-and-forget, so WAIT honestly waits for the last fence
    /// that referenced this GEM (Linux `virtio_gpu_wait_ioctl` →
    /// `dma_resv_wait_timeout`); expiry and the NOWAIT probe both report
    /// `-EBUSY` while the host is still working, so callers can simply retry.
    /// Handles never submitted (dumb buffers, created-but-idle GEMs) have no
    /// fence and report idle.
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
        if let Some(resource) = &resource {
            let last_fence = resource.last_fence.load(Ordering::Acquire);
            if last_fence != 0 {
                if w.flags & VIRTGPU_WAIT_NOWAIT != 0 {
                    let done = with_virgl(|virgl| virgl.fence_completed(last_fence))?;
                    if !done {
                        return Err(VfsError::ResourceBusy);
                    }
                } else {
                    // Linux `virtio_gpu_wait_ioctl()` waits up to 15 * HZ and
                    // reports -EBUSY when the fence is still pending — the
                    // same errno as the NOWAIT probe — so callers treat the
                    // expiry as "busy, retry" instead of a hard failure. Map
                    // the bounded-wait timeout onto that errno. The bound
                    // itself is the driver's 5s stall-recovery window; Linux
                    // simply trusts the host and has no shorter bound. The
                    // wait sleeps OUTSIDE the device control lock (the old
                    // in-driver wait spun under it), so the completion pump
                    // keeps running while the caller blocks here.
                    ax_gpu::virgl_wait_fence(last_fence, ax_gpu::GPU_WAIT_TIMEOUT)
                        .map_err(map_gpu_err)
                        .map_err(|err| {
                            if matches!(err, VfsError::TimedOut) {
                                VfsError::ResourceBusy
                            } else {
                                err
                            }
                        })?;
                }
            }
        }
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
            // A venus context's HOST3D blob lives in the hostmem window; the
            // virgl flavour below keeps its guest shadow allocation.
            let ctx_capset = file.context.lock().as_ref().map(|ctx| ctx.capset_id);
            if ctx_capset == Some(VIRTGPU_DRM_CAPSET_VENUS) {
                return self.handle_venus_resource_create_blob(file, current, arg, b);
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
        let device_handle = with_virgl(|virgl| {
            virgl.create_blob(
                context,
                rdif_gpu::BlobDescriptor {
                    memory: b.blob_mem,
                    flags: b.blob_flags,
                    size: b.size,
                    id: b.blob_id,
                },
                backing,
                &cmd_buf,
            )
        })?;
        let res_handle = match with_virgl(|virgl| virgl.command_resource_id(device_handle)) {
            Ok(id) => id,
            Err(error) => {
                let _ = release_gpu_buffer_and_drain(device_handle);
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
            mapped_blob: Mutex::new(None),
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

    /// `VIRTGPU_RESOURCE_CREATE_BLOB` for a venus (`capset 4`) context.
    ///
    /// The blob's backing lives in the hostmem window: the resource is
    /// created with no guest backing (`HOST3D`), sized up to the window
    /// (no dumb-buffer cap — renderer memory pools are megabytes), and a
    /// mappable blob is mapped into a 16 KiB-aligned BAR slot right away
    /// unless the caller deferred mapping. The synthetic mmap key handed
    /// back by `VIRTGPU_MAP` resolves to that slot's guest-physical range.
    fn handle_venus_resource_create_blob(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
        mut b: DrmVirtgpuResourceCreateBlob,
    ) -> VfsResult<usize> {
        // Stricter than Linux `verify_blob()` (which forwards `pad` and
        // `blob_hints` to the device untouched): a nonzero pad or an
        // undefined hint bit is rejected here instead of reaching the host.
        if b.pad != 0 || b.blob_hints & !DRM_VIRTGPU_BLOB_FLAG_HINT_DEFER_MAPPING != 0 {
            return Err(VfsError::InvalidInput);
        }
        let hostmem =
            with_virgl(|virgl| Ok(virgl.host_memory_region()))?.ok_or(VfsError::InvalidInput)?;
        if b.size == 0 || b.size > hostmem.length {
            return Err(VfsError::InvalidInput);
        }
        // The context owns the blob; venus requires CONTEXT_INIT first.
        let context = {
            let guard = file.context.lock();
            match guard.as_ref() {
                Some(ctx) if ctx.capset_id == VIRTGPU_DRM_CAPSET_VENUS => ctx.ctx_id,
                _ => return Err(VfsError::InvalidInput),
            }
        };

        // Optional initialization command stream, submitted before the blob
        // is created (the control queue is FIFO, so ordering holds).
        // Bound the size before `vm_load` allocates the whole buffer: the
        // u32 `cmd_size` would otherwise let one ioctl demand a multi-GiB
        // kernel allocation, and a null `cmd` with a nonzero size is the
        // same malformed request the virgl flavour rejects above.
        if b.cmd_size as usize > MAX_VENUS_COMMAND_BYTES || (b.cmd_size > 0 && b.cmd == 0) {
            return Err(VfsError::InvalidInput);
        }
        let init_cmd: Vec<u8> = if b.cmd_size > 0 {
            vm_load(current, b.cmd as *const u8, b.cmd_size as usize)
                .map_err(|_| VfsError::BadAddress)?
        } else {
            Vec::new()
        };

        // When the device negotiated a blob alignment requirement, Linux
        // `verify_blob()` rejects unaligned sizes with EINVAL.
        let negotiated_align = with_virgl(|virgl| Ok(virgl.blob_alignment()))?;
        if negotiated_align.is_some_and(|align| !b.size.is_multiple_of(u64::from(align))) {
            return Err(VfsError::InvalidInput);
        }
        // Slot granularity: the negotiated alignment floored at 16 KiB so
        // the host-side subsection (and every EPT subsection the hvf
        // listener derives from it) stays 16 KiB-aligned; see
        // [`BAR_SLOT_ALIGN`]. The wire size is rounded up to it.
        let slot_granularity = negotiated_align
            .map(u64::from)
            .map_or(BAR_SLOT_ALIGN, |align| align.max(BAR_SLOT_ALIGN));
        let wire_size = b.size.div_ceil(slot_granularity) * slot_granularity;
        let device_handle = with_virgl(|virgl| {
            virgl.create_blob(
                Some(context),
                rdif_gpu::BlobDescriptor {
                    memory: b.blob_mem,
                    flags: b.blob_flags,
                    size: wire_size,
                    id: b.blob_id,
                },
                None,
                &init_cmd,
            )
        })?;
        let res_handle = match with_virgl(|virgl| virgl.command_resource_id(device_handle)) {
            Ok(id) => id,
            Err(error) => {
                let _ = release_gpu_buffer_and_drain(device_handle);
                return Err(error);
            }
        };

        // Map every mappable blob into the hostmem window right away. The
        // DEFER_MAPPING hint is a guest-side hostmem saving (Linux skips
        // the eager vram map and maps on first mmap); this driver always
        // maps eagerly — with slot recycling the window cost of blobs that
        // are never mmapped is reclaimed at destroy, and there is no
        // lazy-map path that would make a deferred blob mappable later.
        let mut mapped = None;
        if b.blob_flags & VIRTGPU_BLOB_FLAG_USE_MAPPABLE != 0 {
            let (bar_offset, slot_len) =
                reserve_bar_slot(hostmem.length, wire_size, slot_granularity)?;
            let map_info = match with_virgl(|virgl| virgl.map_blob(device_handle, bar_offset)) {
                Ok(info) => info,
                Err(error) => {
                    // Return the freshly reserved slot before failing.
                    release_bar_slot(bar_offset, slot_len);
                    let _ = release_gpu_buffer_and_drain(device_handle);
                    return Err(error);
                }
            };
            mapped = Some(MappedBlobBar {
                phys: hostmem.base + bar_offset,
                size: b.size,
                bar_offset,
                slot_len,
                mmap_key: self.alloc_blob_mmap_key(),
                map_info,
            });
        }

        let bo_handle = self.next_dumb_handle.fetch_add(1, Ordering::Relaxed);
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
            mapped_blob: Mutex::new(mapped),
        });
        // No CTX_ATTACH here: a venus blob is bound to its context by the
        // create command itself, and the context teardown frees it.

        b.bo_handle = bo_handle;
        b.res_handle = res_handle;
        let ptr = arg as *mut DrmVirtgpuResourceCreateBlob;
        if ptr.vm_write(current, b).is_err() {
            return Err(VfsError::BadAddress);
        }
        self.gpu_resources.lock().insert(res_handle, resource);

        Ok(0)
    }

    /// `VIRTGPU_EXECBUFFER` for a venus (`capset 4`) context.
    ///
    /// Ring-based submission with timeline syncobjs: out-syncobjs advance
    /// only when the host retired the submission's fence (`pending_retires`),
    /// and in-syncobjs are resolved and waited on inline before the submit —
    /// Linux `virtio_gpu_parse_deps` + `virtio_gpu_do_fence_wait`. The wait
    /// is real, not an FIFO assumption: ring batches retire out of order, so
    /// enqueue order alone does not order execution.
    fn handle_venus_execbuffer(
        &self,
        file: &Card0File,
        current: &UserTaskRef,
        arg: usize,
    ) -> StarryResult<usize> {
        let ptr = arg as *mut DrmVirtgpuExecbuffer;
        let eb: DrmVirtgpuExecbuffer = ptr.vm_read(current).map_err(|_| VfsError::BadAddress)?;
        // sync_file fence fds are not modelled on the venus face (venus uses
        // the syncobj path exclusively); ring selection is.
        if eb.flags & !VIRTGPU_EXECBUF_RING_IDX != 0 {
            return Err(StarryError::InvalidInput);
        }
        // size == 0 is legal for ring-based contexts: the commands live in
        // the ring buffer and the EXECBUFFER only kicks the host (Linux
        // passes the empty stream through to SUBMIT_3D verbatim).
        if eb.size as usize > MAX_VENUS_COMMAND_BYTES {
            return Err(StarryError::InvalidInput);
        }

        let (context, num_rings) = {
            let guard = file.context.lock();
            match guard.as_ref() {
                Some(ctx) if ctx.capset_id == VIRTGPU_DRM_CAPSET_VENUS => {
                    (ctx.ctx_id, ctx.num_rings)
                }
                _ => return Err(StarryError::InvalidInput),
            }
        };
        let ring_idx = if eb.flags & VIRTGPU_EXECBUF_RING_IDX != 0 {
            if eb.ring_idx >= num_rings {
                return Err(StarryError::InvalidInput);
            }
            eb.ring_idx as u8
        } else {
            0
        };

        // Validate referenced bo handles (Linux uses them for reservations;
        // venus blobs are context-owned since their CREATE_BLOB).
        let mut resources = Vec::with_capacity(eb.num_bo_handles as usize);
        if eb.num_bo_handles > 0 {
            if eb.num_bo_handles > 256 || eb.bo_handles == 0 {
                return Err(StarryError::InvalidInput);
            }
            let handles: Vec<u32> = vm_load(
                current,
                eb.bo_handles as *const u32,
                eb.num_bo_handles as usize,
            )
            .map_err(|_| VfsError::BadAddress)?;
            for handle in handles {
                let resource = self
                    .resource_for_handle(file, handle)
                    .ok_or(VfsError::NotFound)?;
                resources.push(resource);
            }
        }

        // Read the out-syncobj list before submitting; their points get
        // signalled once the fence retires. Linux `parse_post_deps` allows
        // no per-entry flags on the out side.
        const MAX_EXEC_SYNCOBJS: u32 = 1024;
        let syncobj_stride = if eb.syncobj_stride == 0 {
            core::mem::size_of::<DrmVirtgpuExecbufferSyncobj>()
        } else {
            eb.syncobj_stride as usize
        };
        if syncobj_stride < core::mem::size_of::<DrmVirtgpuExecbufferSyncobj>() {
            return Err(StarryError::InvalidInput);
        }
        let mut out_syncobjs: Vec<(u32, u64)> = Vec::new();
        if eb.num_out_syncobjs > 0 {
            if eb.num_out_syncobjs > MAX_EXEC_SYNCOBJS || eb.out_syncobjs == 0 {
                return Err(StarryError::InvalidInput);
            }
            for i in 0..eb.num_out_syncobjs as usize {
                let addr = eb.out_syncobjs + (i * syncobj_stride) as u64;
                let entry: DrmVirtgpuExecbufferSyncobj =
                    vm_load(current, addr as *const DrmVirtgpuExecbufferSyncobj, 1)
                        .map_err(|_| VfsError::BadAddress)?[0];
                if entry.flags != 0 {
                    return Err(StarryError::InvalidInput);
                }
                // Point 0 addresses the binary view (point 1): Linux
                // `virtio_gpu_process_post_deps` replaces the fence of a
                // point-0 out-syncobj at retire, so the raw 0 must never
                // reach the completion paths, where it would compare as
                // always-reached and never advance the watermark.
                out_syncobjs.push((entry.handle, entry.point.max(1)));
            }
            // Validate the handles up front: a queued submit with a dangling
            // handle would never signal anything once its retire response
            // lands (Linux `parse_post_deps` failure is EINVAL).
            let table = self.syncobjs.lock();
            for (handle, _) in &out_syncobjs {
                if !table.contains_key(&(file.file_id, *handle)) {
                    return Err(StarryError::InvalidInput);
                }
            }
        }

        // In-syncobjs (dependencies): the only legal per-entry flag is RESET
        // (Linux `VIRTGPU_EXECBUF_SYNCOBJ_FLAGS`); RESET entries are reset
        // after the submit completes, like the Linux cleanup stage.
        let mut in_syncobjs: Vec<(u32, u64)> = Vec::new();
        let mut reset_syncobjs: Vec<u32> = Vec::new();
        if eb.num_in_syncobjs > 0 {
            if eb.num_in_syncobjs > MAX_EXEC_SYNCOBJS || eb.in_syncobjs == 0 {
                return Err(StarryError::InvalidInput);
            }
            for i in 0..eb.num_in_syncobjs as usize {
                let addr = eb.in_syncobjs + (i * syncobj_stride) as u64;
                let entry: DrmVirtgpuExecbufferSyncobj =
                    vm_load(current, addr as *const DrmVirtgpuExecbufferSyncobj, 1)
                        .map_err(|_| VfsError::BadAddress)?[0];
                if entry.flags & !VIRTGPU_EXECBUF_SYNCOBJ_FLAGS_RESET != 0 {
                    return Err(StarryError::InvalidInput);
                }
                if entry.flags & VIRTGPU_EXECBUF_SYNCOBJ_FLAGS_RESET != 0 {
                    reset_syncobjs.push(entry.handle);
                }
                in_syncobjs.push((entry.handle, entry.point));
            }
            // An unknown dependency handle is ENOENT (Linux `find_fence`).
            let table = self.syncobjs.lock();
            for (handle, _) in &in_syncobjs {
                if !table.contains_key(&(file.file_id, *handle)) {
                    return Err(VfsError::NotFound.into());
                }
            }
        }

        // Copy the command stream. Empty streams (ring kicks) skip this.
        let cmd: Vec<u8> = if eb.size == 0 {
            Vec::new()
        } else if eb.command == 0 {
            return Err(StarryError::InvalidInput);
        } else {
            vm_load(current, eb.command as *const u8, eb.size as usize)
                .map_err(|_| VfsError::BadAddress)?
        };

        // Wait out the dependencies before the submit: each in-syncobj
        // point must already be signalled (Linux blocks in the ioctl on the
        // dependency fence; same-device fences could skip, but every venus
        // fence here is a ring batch the host may not have consumed yet, so
        // enqueue order does not order execution). A deliverable signal
        // aborts the wait with EINTR (Linux -ERESTARTSYS) and an unbacked
        // point fails with EINVAL before parking.
        for &(handle, point) in &in_syncobjs {
            self.wait_syncobj_dependency(file.file_id, current, handle, point)?;
        }

        // Linux fences every venus EXECBUFFER (the ring idx rides on the
        // fence): the host defers the SUBMIT_3D response to fence retire
        // regardless, so an unfenced submit would block forever on the
        // synchronous round-trip.
        let completion = with_virgl(|virgl| virgl.submit_ring(context, &cmd, ring_idx))?;
        let fence_id = completion_fence(completion);
        for resource in &resources {
            resource.last_fence.store(fence_id, Ordering::Release);
        }

        // The out-syncobjs signal when the HOST's retire response for this
        // fence lands (`pump_retired_fences`), not at submit time: the guest
        // reads a retired fence as "the host consumed my submission", so an
        // early signal lets it lap the venus ring and corrupt the host
        // command stream. A synchronously-completed submit (fence id 0 —
        // the driver answered without a pending retire) has no retire to
        // wait for, so its out-syncobjs advance immediately (Linux
        // `virtio_gpu_process_post_deps` attaches the fence unconditionally,
        // whatever the submit mode was).
        if !out_syncobjs.is_empty() && fence_id != 0 {
            let mut woken = Vec::new();
            self.pending_retires.lock().push_back(super::syncobj::PendingRetire::new(
                fence_id,
                file.file_id,
                out_syncobjs.clone(),
            ));
            // Attaching the fences satisfies every WAIT_AVAILABLE entry on
            // these points (Linux re-runs the eventfd entries from
            // drm_syncobj_add_point); completion entries stay parked until
            // the retire lands. Registering first closes the race: a
            // concurrent EVENTFD ioctl either lands in the scan or sees the
            // backing through `pending_fence_for` and fires itself.
            self.fire_available_waiters(file.file_id, &out_syncobjs, &mut woken);
            // Arm the completion-side pump: a client that parks in
            // `poll(eventfd)` after this submit issues no further ioctls,
            // so the pump task is what observes the retire for it.
            super::syncobj::kick_retire_pump();
            for eventfd in woken {
                let _ = eventfd.signal_kernel(1);
            }
        } else if !out_syncobjs.is_empty() {
            let mut woken = Vec::new();
            for (handle, point) in out_syncobjs {
                self.signal_syncobj_point(file.file_id, handle, point, &mut woken);
            }
            for eventfd in woken {
                let _ = eventfd.signal_kernel(1);
            }
        }
        // EXECBUFFER is a fire-and-forget transaction: one boundary notify
        // delivers the submission — Linux `virtio_gpu_notify()` at the ioctl
        // end, exactly like the virgl path above. Without it, a kick that
        // carries no out-syncobj has no other delivery path: the retire pump
        // only notifies while a pending retire exists, so the doorbell would
        // stay in the avail ring with the host idle and every waiter on the
        // batch's effects would sleep forever.
        with_virgl(|virgl| {
            virgl.ctrl_notify();
            Ok(())
        })?;
        self.pump_retired_fences();
        // RESET-flagged in-syncobjs are cleared after the submit (Linux
        // `virtio_gpu_reset_syncobjs` in the cleanup stage).
        for &handle in &reset_syncobjs {
            if let Ok(states) = self.lookup_syncobjs(file.file_id, &[handle]) {
                states[0].lock().signaled_point = 0;
            }
        }
        Ok(0)
    }
}
