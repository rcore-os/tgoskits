//! EFI system partition access for the launcher and OTA loader.

use alloc::vec::Vec;

use sha2::{Digest, Sha256};
use uefi::{
    Result, Status, boot,
    proto::{
        BootPolicy,
        device_path::{DevicePath, build},
        loaded_image::LoadedImage,
        media::{
            file::{File, FileAttribute, FileMode, RegularFile},
            fs::SimpleFileSystem,
        },
    },
};

use super::{Slot, State, state};

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

fn slot_path(slot: Slot) -> &'static uefi::CStr16 {
    match slot {
        Slot::A => uefi::cstr16!("\\EFI\\AXLOADER\\A.EFI"),
        Slot::B => uefi::cstr16!("\\EFI\\AXLOADER\\B.EFI"),
    }
}

pub fn load_slot(slot: Slot) -> Result<uefi::Handle> {
    let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())?;
    let device = image.device().ok_or(Status::UNSUPPORTED)?;
    drop(image);
    let device_path = boot::open_protocol_exclusive::<DevicePath>(device)?;
    let mut file_nodes = Vec::new();
    let file_path = build::DevicePathBuilder::with_vec(&mut file_nodes)
        .push(&build::media::FilePath {
            path_name: slot_path(slot),
        })
        .map_err(|_| Status::INVALID_PARAMETER)?
        .finalize()
        .map_err(|_| Status::INVALID_PARAMETER)?;
    let full = device_path
        .append_path(file_path)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    boot::load_image(
        boot::image_handle(),
        boot::LoadImageSource::FromDevicePath {
            device_path: &full,
            boot_policy: BootPolicy::ExactMatch,
        },
    )
}

fn record_path(index: usize) -> &'static uefi::CStr16 {
    match index {
        0 => uefi::cstr16!("\\EFI\\AXLOADER\\STATE0.BIN"),
        _ => uefi::cstr16!("\\EFI\\AXLOADER\\STATE1.BIN"),
    }
}

pub struct OtaDisk {
    fs: boot::ScopedProtocol<SimpleFileSystem>,
}

pub struct InactiveWriter {
    file: RegularFile,
    slot: Slot,
    written: usize,
    expected_size: usize,
}

impl InactiveWriter {
    pub fn write(&mut self, chunk: &[u8]) -> Result<()> {
        let target = self
            .written
            .checked_add(chunk.len())
            .filter(|length| *length <= self.expected_size)
            .ok_or(Status::BAD_BUFFER_SIZE)?;
        // RegularFile::write guarantees the complete buffer on success. Its
        // error payload carries the partial byte count, so a failed upload
        // never reaches finish() or changes the active slot.
        self.file
            .write(chunk)
            .map_err(|error| error.to_err_without_payload())?;
        self.written = target;
        Ok(())
    }

    pub fn finish(mut self, disk: &mut OtaDisk, expected: &[u8; 32]) -> Result<Slot> {
        if self.written != self.expected_size {
            return Err(Status::END_OF_FILE.into());
        }
        self.file.flush()?;
        let slot = self.slot;
        drop(self);
        if &disk.hash_slot(slot)? != expected {
            return Err(Status::CRC_ERROR.into());
        }
        disk.verify_efi_image(slot)?;
        Ok(slot)
    }
}

impl OtaDisk {
    pub fn open() -> Result<Self> {
        boot::get_image_file_system(boot::image_handle()).map(|fs| Self { fs })
    }

    fn file(&mut self, path: &uefi::CStr16, mode: FileMode) -> Result<RegularFile> {
        self.fs
            .open_volume()?
            .open(path, mode, FileAttribute::empty())?
            .into_regular_file()
            .ok_or(Status::INVALID_PARAMETER.into())
    }

    fn record(&mut self, index: usize) -> [u8; state::RECORD_SIZE] {
        let mut result = [0; state::RECORD_SIZE];
        let Ok(mut file) = self.file(record_path(index), FileMode::Read) else {
            return result;
        };
        let mut buf = [0; state::RECORD_SIZE + 1];
        if let Ok(state::RECORD_SIZE) = file.read(&mut buf) {
            result.copy_from_slice(&buf[..state::RECORD_SIZE]);
        }
        result
    }

    pub fn load(&mut self) -> Result<(State, usize)> {
        let a = self.record(0);
        let b = self.record(1);
        state::newest(&a, &b).map_err(|_| Status::COMPROMISED_DATA.into())
    }

    /// The caller retains the previous record and may proceed only after this
    /// function has flushed and read back the complete next record.
    pub fn commit(&mut self, previous_index: usize, next: &State) -> Result<usize> {
        let target = 1 - previous_index;
        let path = record_path(target);
        let mut file = match self.file(path, FileMode::ReadWrite) {
            Ok(file) => file,
            Err(error) if error.status() == Status::NOT_FOUND => {
                self.file(path, FileMode::CreateReadWrite)?
            }
            Err(error) => return Err(error),
        };
        file.set_position(0)?;
        file.write(&next.encode())
            .map_err(|error| error.to_err_without_payload())?;
        file.flush()?;
        drop(file);
        if self.record(target) != next.encode() {
            return Err(Status::CRC_ERROR.into());
        }
        Ok(target)
    }

    pub fn hash_slot(&mut self, slot: Slot) -> Result<[u8; 32]> {
        let mut file = self.file(slot_path(slot), FileMode::Read)?;
        let mut hasher = Sha256::new();
        let mut buf = [0_u8; 8192];
        let mut total = 0_usize;
        loop {
            let length = file.read(&mut buf)?;
            if length == 0 {
                break;
            }
            total = total
                .checked_add(length)
                .filter(|size| *size <= MAX_IMAGE_BYTES)
                .ok_or(Status::BAD_BUFFER_SIZE)?;
            hasher.update(&buf[..length]);
        }
        if total == 0 {
            return Err(Status::LOAD_ERROR.into());
        }
        Ok(hasher.finalize().into())
    }

    fn verify_efi_image(&mut self, slot: Slot) -> Result<()> {
        let mut file = self.file(slot_path(slot), FileMode::Read)?;
        let mut dos = [0_u8; 64];
        if file.read(&mut dos)? != dos.len() || &dos[..2] != b"MZ" {
            return Err(Status::LOAD_ERROR.into());
        }
        let offset = u32::from_le_bytes(dos[0x3c..0x40].try_into().unwrap()) as usize;
        if offset
            .checked_add(94)
            .is_none_or(|end| end > MAX_IMAGE_BYTES)
        {
            return Err(Status::LOAD_ERROR.into());
        }
        file.set_position(offset as u64)?;
        let mut pe = [0_u8; 94];
        if file.read(&mut pe)? != pe.len()
            || &pe[..4] != b"PE\0\0"
            || pe[4..6] != [0x64, 0x86]
            || pe[24..26] != [0x0b, 0x02]
            || pe[92..94] != [10, 0]
        {
            return Err(Status::LOAD_ERROR.into());
        }
        Ok(())
    }

    pub fn start_inactive(
        &mut self,
        state: &State,
        expected_size: usize,
    ) -> Result<InactiveWriter> {
        if state.pending.is_some() {
            return Err(Status::ACCESS_DENIED.into());
        }
        let slot = state.active.other();
        if expected_size == 0 || expected_size > MAX_IMAGE_BYTES {
            return Err(Status::BAD_BUFFER_SIZE.into());
        }
        let path = slot_path(slot);
        // Only the inactive slot may be removed. The caller holds the verified
        // stable record and never passes its active slot to this method.
        let mut root = self.fs.open_volume()?;
        if let Ok(file) = root.open(path, FileMode::ReadWrite, FileAttribute::empty()) {
            file.delete()?;
        }
        let file = root
            .open(path, FileMode::CreateReadWrite, FileAttribute::empty())?
            .into_regular_file()
            .ok_or(Status::INVALID_PARAMETER)?;
        Ok(InactiveWriter {
            file,
            slot,
            written: 0,
            expected_size,
        })
    }
}
