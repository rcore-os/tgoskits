//! Per-core capability snapshot (seeds the spec §11.2 matrix). PR-1: copy/fill only.
use crate::{RgaHardwareVersion, RgaVersion};

#[derive(Debug, Clone, Copy)]
pub struct CoreCapabilities {
    pub generation: RgaVersion,
    pub version: RgaHardwareVersion,
    pub max_dimension: u32,
    pub copy: bool,
    pub fill: bool,
}

impl CoreCapabilities {
    pub fn detect(generation: RgaVersion, version: RgaHardwareVersion) -> Self {
        Self {
            generation,
            version,
            max_dimension: 8192,
            copy: true,
            fill: matches!(generation, RgaVersion::Rga2),
        }
    }
}
