//! Dry-run command buffer construction for small RGA bring-up tests.
//!
//! The encoder builds the 32-word mode command block that old Rockchip RGA
//! hardware consumes through `RGA_CMD_BASE`. It does not submit the command to
//! hardware; board glue must still handle DMA allocation, cache sync, IRQ or
//! polling, clocks, and power.

use super::registers;

pub const MIN_DIMENSION: u32 = 34;
pub const MAX_DIMENSION: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    AddressNotAligned,
    AddressTooLarge,
    InvalidDimensions,
    InvalidStride,
    SizeMismatch,
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Abgr8888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Abgr8888 => 4,
        }
    }

    const fn hw_format(self) -> u32 {
        match self {
            Self::Abgr8888 => registers::COLOR_FMT_ABGR8888,
        }
    }

    const fn color_swap(self) -> u32 {
        registers::COLOR_NONE_SWAP
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLayout {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub format: PixelFormat,
}

impl ImageLayout {
    pub const fn new(width: u32, height: u32, stride_bytes: u32, format: PixelFormat) -> Self {
        Self {
            width,
            height,
            stride_bytes,
            format,
        }
    }

    fn validate(self) -> Result<()> {
        if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&self.width)
            || !(MIN_DIMENSION..=MAX_DIMENSION).contains(&self.height)
        {
            return Err(Error::InvalidDimensions);
        }

        let min_stride = self
            .width
            .checked_mul(self.format.bytes_per_pixel())
            .ok_or(Error::InvalidStride)?;
        if self.stride_bytes < min_stride || !self.stride_bytes.is_multiple_of(4) {
            return Err(Error::InvalidStride);
        }

        let stride_words = self.stride_words();
        if stride_words > 0x03ff {
            return Err(Error::InvalidStride);
        }

        Ok(())
    }

    const fn stride_words(self) -> u32 {
        self.stride_bytes / 4
    }
}

/// RGA-visible buffer mapping used by the dry-run encoder.
///
/// `descriptor_dma` follows the old RGA local-MMU command format: command
/// words store a DMA address for a descriptor table, shifted right by 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferMapping {
    pub descriptor_dma: u64,
    pub y_offset: u32,
    pub u_offset: u32,
    pub v_offset: u32,
}

impl BufferMapping {
    pub const fn new(descriptor_dma: u64) -> Self {
        Self {
            descriptor_dma,
            y_offset: 0,
            u_offset: 0,
            v_offset: 0,
        }
    }

    fn descriptor_base_word(self) -> Result<u32> {
        if self.descriptor_dma & 0xf != 0 {
            return Err(Error::AddressNotAligned);
        }
        let shifted = self.descriptor_dma >> 4;
        if shifted > u32::MAX as u64 {
            return Err(Error::AddressTooLarge);
        }
        Ok(shifted as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageBuffer {
    pub mapping: BufferMapping,
    pub layout: ImageLayout,
}

impl ImageBuffer {
    pub const fn new(mapping: BufferMapping, layout: ImageLayout) -> Self {
        Self { mapping, layout }
    }

    fn validate(self) -> Result<()> {
        self.mapping.descriptor_base_word()?;
        self.layout.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyCommand {
    pub src: ImageBuffer,
    pub dst: ImageBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillCommand {
    pub dst: ImageBuffer,
    pub color: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBuffer {
    words: [u32; registers::CMD_BUFFER_WORDS],
}

impl CommandBuffer {
    pub const fn zeroed() -> Self {
        Self {
            words: [0; registers::CMD_BUFFER_WORDS],
        }
    }

    pub fn copy(command: CopyCommand) -> Result<Self> {
        command.src.validate()?;
        command.dst.validate()?;
        if command.src.layout.width != command.dst.layout.width
            || command.src.layout.height != command.dst.layout.height
        {
            return Err(Error::SizeMismatch);
        }

        let mut buffer = Self::zeroed();
        buffer.set_common_dst(command.dst)?;
        buffer.set_src(command.src)?;
        buffer.set_register(
            registers::MMU_SRC1_BASE,
            command.dst.mapping.descriptor_base_word()?,
        );
        buffer.set_register(
            registers::MMU_CTRL1,
            registers::MMU_SRC_ENABLE | registers::MMU_SRC1_ENABLE | registers::MMU_DST_ENABLE,
        );
        buffer.set_register(
            registers::MODE_CTRL,
            encode_mode(
                registers::MODE_RENDER_BITBLT,
                registers::MODE_BITBLT_SRC_TO_DST,
            ),
        );
        Ok(buffer)
    }

    pub fn fill(command: FillCommand) -> Result<Self> {
        command.dst.validate()?;

        let mut buffer = Self::zeroed();
        buffer.set_common_dst(command.dst)?;
        buffer.set_register(registers::SRC_BG_COLOR, command.color);
        buffer.set_register(
            registers::MODE_CTRL,
            encode_mode(registers::MODE_RENDER_RECTANGLE_FILL, 0),
        );
        buffer.set_register(registers::MMU_CTRL1, registers::MMU_DST_ENABLE);
        Ok(buffer)
    }

    pub fn words(&self) -> &[u32; registers::CMD_BUFFER_WORDS] {
        &self.words
    }

    pub fn register(&self, register: usize) -> Option<u32> {
        command_index(register).map(|index| self.words[index])
    }

    fn set_src(&mut self, image: ImageBuffer) -> Result<()> {
        self.set_register(
            registers::MMU_SRC_BASE,
            image.mapping.descriptor_base_word()?,
        );
        self.set_register(registers::SRC_Y_RGB_BASE_ADDR, image.mapping.y_offset);
        self.set_register(registers::SRC_CB_BASE_ADDR, image.mapping.u_offset);
        self.set_register(registers::SRC_CR_BASE_ADDR, image.mapping.v_offset);
        self.set_register(registers::SRC_INFO, encode_src_info(image.layout.format));
        self.set_register(registers::SRC_VIR_INFO, encode_src_vir_info(image.layout));
        self.set_register(registers::SRC_ACT_INFO, encode_src_act_info(image.layout));
        self.set_register(registers::SRC_X_FACTOR, 0);
        self.set_register(registers::SRC_Y_FACTOR, 0);
        Ok(())
    }

    fn set_common_dst(&mut self, image: ImageBuffer) -> Result<()> {
        self.set_register(
            registers::MMU_DST_BASE,
            image.mapping.descriptor_base_word()?,
        );
        self.set_register(registers::DST_Y_RGB_BASE_ADDR, image.mapping.y_offset);
        self.set_register(registers::DST_CB_BASE_ADDR, image.mapping.u_offset);
        self.set_register(registers::DST_CR_BASE_ADDR, image.mapping.v_offset);
        self.set_register(registers::DST_INFO, encode_dst_info(image.layout.format));
        self.set_register(registers::DST_VIR_INFO, encode_dst_vir_info(image.layout));
        self.set_register(registers::DST_ACT_INFO, encode_dst_act_info(image.layout));
        Ok(())
    }

    fn set_register(&mut self, register: usize, value: u32) {
        if let Some(index) = command_index(register) {
            self.words[index] = value;
        }
    }
}

fn command_index(register: usize) -> Option<usize> {
    let offset = register.checked_sub(registers::MODE_BASE)?;
    if offset % core::mem::size_of::<u32>() != 0 {
        return None;
    }
    let index = offset / core::mem::size_of::<u32>();
    (index < registers::CMD_BUFFER_WORDS).then_some(index)
}

const fn encode_mode(render: u32, bitblt: u32) -> u32 {
    (render & 0x7) | ((bitblt & 0x1) << 3) | (1 << 6)
}

const fn encode_src_info(format: PixelFormat) -> u32 {
    format.hw_format() | (format.color_swap() << 4)
}

const fn encode_dst_info(format: PixelFormat) -> u32 {
    format.hw_format() | (format.color_swap() << 4)
}

const fn encode_src_vir_info(layout: ImageLayout) -> u32 {
    let stride = layout.stride_words();
    (stride & 0x7fff) | ((stride & 0x03ff) << 16)
}

const fn encode_src_act_info(layout: ImageLayout) -> u32 {
    ((layout.width - 1) & 0x1fff) | (((layout.height - 1) & 0x1fff) << 16)
}

const fn encode_dst_vir_info(layout: ImageLayout) -> u32 {
    layout.stride_words() & 0x7fff
}

const fn encode_dst_act_info(layout: ImageLayout) -> u32 {
    ((layout.width - 1) & 0x0fff) | (((layout.height - 1) & 0x0fff) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> ImageLayout {
        ImageLayout::new(64, 48, 256, PixelFormat::Abgr8888)
    }

    #[test]
    fn copy_command_encodes_addresses_and_geometry() {
        let src = ImageBuffer::new(BufferMapping::new(0x1000), layout());
        let dst = ImageBuffer::new(BufferMapping::new(0x2000), layout());

        let command = CommandBuffer::copy(CopyCommand { src, dst }).unwrap();

        assert_eq!(command.register(registers::MMU_SRC_BASE), Some(0x100));
        assert_eq!(command.register(registers::MMU_SRC1_BASE), Some(0x200));
        assert_eq!(command.register(registers::MMU_DST_BASE), Some(0x200));
        assert_eq!(
            command.register(registers::MMU_CTRL1),
            Some(
                registers::MMU_SRC_ENABLE | registers::MMU_SRC1_ENABLE | registers::MMU_DST_ENABLE
            )
        );
        assert_eq!(command.register(registers::SRC_INFO), Some(0));
        assert_eq!(command.register(registers::DST_INFO), Some(0));
        assert_eq!(
            command.register(registers::SRC_VIR_INFO),
            Some(64 | (64 << 16))
        );
        assert_eq!(command.register(registers::DST_VIR_INFO), Some(64));
        assert_eq!(
            command.register(registers::SRC_ACT_INFO),
            Some(63 | (47 << 16))
        );
        assert_eq!(
            command.register(registers::DST_ACT_INFO),
            Some(63 | (47 << 16))
        );
        assert_eq!(
            command.register(registers::MODE_CTRL),
            Some(encode_mode(
                registers::MODE_RENDER_BITBLT,
                registers::MODE_BITBLT_SRC_TO_DST,
            ))
        );
    }

    #[test]
    fn fill_command_encodes_destination_and_color() {
        let dst = ImageBuffer::new(BufferMapping::new(0x3000), layout());

        let command = CommandBuffer::fill(FillCommand {
            dst,
            color: 0xff00_00ff,
        })
        .unwrap();

        assert_eq!(command.register(registers::MMU_DST_BASE), Some(0x300));
        assert_eq!(
            command.register(registers::MMU_CTRL1),
            Some(registers::MMU_DST_ENABLE)
        );
        assert_eq!(command.register(registers::SRC_BG_COLOR), Some(0xff00_00ff));
        assert_eq!(
            command.register(registers::MODE_CTRL),
            Some(encode_mode(registers::MODE_RENDER_RECTANGLE_FILL, 0))
        );
    }

    #[test]
    fn rejects_unaligned_descriptor_address() {
        let src = ImageBuffer::new(BufferMapping::new(0x1001), layout());
        let dst = ImageBuffer::new(BufferMapping::new(0x2000), layout());

        assert_eq!(
            CommandBuffer::copy(CopyCommand { src, dst }),
            Err(Error::AddressNotAligned)
        );
    }

    #[test]
    fn rejects_stride_smaller_than_pixel_row() {
        let bad_layout = ImageLayout::new(64, 48, 128, PixelFormat::Abgr8888);
        let src = ImageBuffer::new(BufferMapping::new(0x1000), bad_layout);
        let dst = ImageBuffer::new(BufferMapping::new(0x2000), layout());

        assert_eq!(
            CommandBuffer::copy(CopyCommand { src, dst }),
            Err(Error::InvalidStride)
        );
    }

    #[test]
    fn rejects_copy_size_mismatch() {
        let src = ImageBuffer::new(BufferMapping::new(0x1000), layout());
        let dst = ImageBuffer::new(
            BufferMapping::new(0x2000),
            ImageLayout::new(80, 48, 320, PixelFormat::Abgr8888),
        );

        assert_eq!(
            CommandBuffer::copy(CopyCommand { src, dst }),
            Err(Error::SizeMismatch)
        );
    }
}
