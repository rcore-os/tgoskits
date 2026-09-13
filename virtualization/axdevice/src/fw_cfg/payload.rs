use alloc::sync::Arc;

/// Architecture-prepared kernel bytes exposed through QEMU fw_cfg.
///
/// x86 bzImage boot requires the real-mode setup and protected-mode kernel in
/// separate selectors. Other architectures use an empty setup and place their
/// complete image in `kernel`.
#[derive(Clone, Debug)]
pub struct FwCfgKernelPayload {
    setup: Arc<[u8]>,
    kernel: Arc<[u8]>,
}

impl FwCfgKernelPayload {
    /// Creates an empty kernel payload.
    pub fn empty() -> Self {
        Self::unsplit(Arc::from(&b""[..]))
    }

    /// Creates a payload whose complete image is exposed as kernel data.
    pub fn unsplit(kernel: Arc<[u8]>) -> Self {
        Self {
            setup: Arc::from(&b""[..]),
            kernel,
        }
    }

    /// Creates a payload from architecture-validated setup and kernel bytes.
    pub const fn split(setup: Arc<[u8]>, kernel: Arc<[u8]>) -> Self {
        Self { setup, kernel }
    }

    pub(crate) fn setup(&self) -> &[u8] {
        &self.setup
    }

    pub(crate) fn kernel(&self) -> &[u8] {
        &self.kernel
    }

    /// Returns the combined image size used for diagnostics.
    pub fn total_len(&self) -> usize {
        self.setup.len().saturating_add(self.kernel.len())
    }
}

impl Default for FwCfgKernelPayload {
    fn default() -> Self {
        Self::empty()
    }
}
