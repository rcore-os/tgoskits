// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
//! Linux DRM type definitions. Source headers:
//! `include/linux/dma-resv.h`
//!
//! `struct dma_resv` is defined in `drm_kapi::types` beside the fence core
//! that operates on it; the layout asserts travel with that definition.

pub use drm_kapi::types::DmaResv;
