use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

use rdif_gpu::{Backing, BufferHandle, Completion, PixelFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputId(u32);

impl OutputId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn id(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Zero means the device does not report a refresh rate.
    pub refresh_millihz: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub id: OutputId,
    pub name: String,
    pub kind: OutputKind,
    pub connected: bool,
    pub physical_size_mm: Option<(u32, u32)>,
    pub modes: Vec<Mode>,
    pub preferred_mode: Option<Mode>,
    pub formats: Vec<PixelFormat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Unknown,
    Virtual,
    Internal,
    Hdmi,
    DisplayPort,
    Vga,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Scanout may refer to a GPU-owned resource or a display-only allocation.
#[derive(Clone)]
pub enum ScanoutBuffer {
    Gpu(BufferHandle),
    Backing(Arc<dyn Backing>),
}

impl fmt::Debug for ScanoutBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gpu(handle) => f.debug_tuple("Gpu").field(handle).finish(),
            Self::Backing(backing) => f
                .debug_struct("Backing")
                .field("len", &backing.len())
                .field("domain_id", &backing.domain_id())
                .finish(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Framebuffer {
    pub buffer: ScanoutBuffer,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub offset: usize,
    pub format: PixelFormat,
}

impl Framebuffer {
    /// Minimum allocation size for the visible plane, or `None` on overflow
    /// or impossible geometry. The display driver still checks its own limits.
    pub fn required_len(&self) -> Option<usize> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let row = (self.width as usize).checked_mul(self.format.bytes_per_pixel())?;
        if (self.stride as usize) < row {
            return None;
        }
        self.offset
            .checked_add((self.height as usize - 1).checked_mul(self.stride as usize)?)?
            .checked_add(row)
    }
}

/// Complete state of one output. `framebuffer: None` disables scanout.
#[derive(Debug, Clone)]
pub struct DisplayState {
    pub output: OutputId,
    pub mode: Option<Mode>,
    pub framebuffer: Option<Framebuffer>,
    pub damage: Vec<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayEvent {
    OutputChanged(OutputId),
    CommitCompleted {
        output: OutputId,
        completion: Completion,
    },
}
