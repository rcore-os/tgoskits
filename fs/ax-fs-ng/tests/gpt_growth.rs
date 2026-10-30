//! GPT growth contract tests using sfdisk fixtures and independent CRC edits.
//! Fault injection models a half-written sector or a failed flush at each write
//! boundary; reopening must retain an identifiable, retryable partition layout.

use std::{
    fs::{self, File},
    io::Write,
    process::{Command, Stdio},
};

use ax_fs_ng::volume::{
    BlockReader, BlockWriter, DiskId, Error, PartitionId, Result, grow_partition, partition_growth,
    scan_volumes,
};

/// In-memory whole disk with deterministic read, torn-write and flush failures.
#[derive(Clone)]
struct Disk {
    /// Complete medium contents; writes mutate this snapshot even on failure.
    bytes: Vec<u8>,
    /// One-based write/flush operation to interrupt, or None for normal I/O.
    fail_at: Option<usize>,
    /// Number of write/flush attempts used to select an interruption boundary.
    operations: usize,
    /// Optional device LBA whose reads fail without altering the stored bytes.
    bad_read: Option<u64>,
}

impl BlockReader for Disk {
    /// Returns the fixture's fixed 512-byte logical sector size.
    fn block_size(&self) -> usize {
        512
    }
    /// Returns whole sectors in the backing byte snapshot.
    fn num_blocks(&self) -> u64 {
        self.bytes.len() as u64 / 512
    }
    /// Copies LBA `block` into one-sector `output`, unless its read is faulted.
    fn read_block(&mut self, block: u64, output: &mut [u8]) -> Result<()> {
        if self.bad_read == Some(block) {
            return Err(Error::Reader);
        }
        output.copy_from_slice(
            self.bytes
                .get(block as usize * 512..block as usize * 512 + 512)
                .ok_or(Error::OutOfRange)?,
        );
        Ok(())
    }
}

impl BlockWriter for Disk {
    /// Writes one-sector `input` at LBA `block`; the selected fault persists
    /// only the first half-sector and returns a writer error.
    fn write_block(&mut self, block: u64, input: &[u8]) -> Result<()> {
        self.operations += 1;
        let first = block as usize * 512;
        if self.fail_at == Some(self.operations) {
            self.bytes[first..first + 256].copy_from_slice(&input[..256]);
            return Err(Error::Writer);
        }
        self.bytes[first..first + 512].copy_from_slice(input);
        Ok(())
    }
    /// Records a durability boundary and fails at the configured operation.
    fn flush(&mut self) -> Result<()> {
        self.operations += 1;
        if self.fail_at == Some(self.operations) {
            return Err(Error::Writer);
        }
        Ok(())
    }
}

/// Creates an 8 MiB sfdisk GPT, adds 256-byte entry extensions, then exposes it
/// as a 16 MiB medium with its backup still at the original image end.
fn fixture() -> Disk {
    let path = std::env::temp_dir().join(format!("gpt-native-grow-{}.img", std::process::id()));
    File::create(&path)
        .unwrap()
        .set_len(8 * 1024 * 1024)
        .unwrap();
    let mut command = Command::new("sfdisk")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    command
        .stdin
        .take()
        .unwrap()
        .write_all(b"label: gpt\nstart=2048,size=4096,type=linux,name=rootfs\n")
        .unwrap();
    let output = command.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    edit_table(&path, "extended");
    let mut bytes = fs::read(&path).unwrap();
    bytes.resize(16 * 1024 * 1024, 0);
    fs::remove_file(path).unwrap();
    Disk {
        bytes,
        fail_at: None,
        operations: 0,
        bad_read: None,
    }
}

/// Edits the image at `path` using independent Python CRCs. `mode` selects an
/// extended-entry or divergent-copy fixture; command failure fails the test.
fn edit_table(path: &std::path::Path, mode: &str) {
    // Python's independent CRC implementation constructs valid extended GPT
    // entries and divergent-copy fixtures without calling the production code.
    let script = r#"
import sys, struct, binascii
path, mode = sys.argv[1:]
with open(path, 'r+b') as disk:
    disk.seek(512); primary = bytearray(disk.read(512))
    tail = struct.unpack_from('<Q', primary, 32)[0]
    disk.seek(tail * 512); backup = bytearray(disk.read(512))
    count, size = struct.unpack_from('<II', primary, 80)
    entries_lba = struct.unpack_from('<Q', primary, 72)[0]
    disk.seek(entries_lba * 512); old = disk.read(count * size)
    if mode == 'stale-tail':
        # Reflashing the small image preserves its matching primary/backup pair
        # but leaves an independently valid GPT from an older disk at the end.
        disk.seek(0, 2); physical_tail = disk.tell() // 512 - 1
        blocks = (len(old) + 511) // 512
        backup[56] ^= 1
        struct.pack_into('<Q', backup, 24, physical_tail)
        struct.pack_into('<Q', backup, 48, physical_tail - blocks - 1)
        struct.pack_into('<Q', backup, 72, physical_tail - blocks)
        struct.pack_into('<I', backup, 16, 0)
        struct.pack_into('<I', backup, 16, binascii.crc32(backup[:struct.unpack_from('<I', backup, 12)[0]]))
        disk.seek((physical_tail - blocks) * 512); disk.write(old)
        disk.seek(physical_tail * 512); disk.write(backup)
        sys.exit(0)
    if mode == 'extended':
        size = 256
        entries = bytearray(count * size)
        for index in range(count): entries[index*size:index*size+128] = old[index*128:(index+1)*128]
        entries[128:256] = bytes(range(128))
        struct.pack_into('<Q', entries, 48, 4)
    else:
        entries = bytearray(old)
        if mode == 'small-root': struct.pack_into('<Q', entries, 40, 6143)
    blocks = (len(entries) + 511) // 512
    last = tail - blocks - 1
    for header, current, start in [(primary, 1, entries_lba), (backup, tail, tail-blocks)]:
        if mode == 'extended': struct.pack_into('<Q', header, 40, max(struct.unpack_from('<Q', header, 40)[0], entries_lba + blocks))
        struct.pack_into('<Q', header, 48, last)
        struct.pack_into('<Q', header, 72, start)
        struct.pack_into('<I', header, 84, size)
        struct.pack_into('<I', header, 88, binascii.crc32(entries))
        if mode == 'conflict' and current != 1: header[56] ^= 1
        struct.pack_into('<I', header, 16, 0)
        struct.pack_into('<I', header, 16, binascii.crc32(header[:struct.unpack_from('<I', header, 12)[0]]))
        disk.seek(start * 512); disk.write(entries)
        disk.seek(current * 512); disk.write(header)
"#;
    let output = Command::new("python3")
        .args(["-c", script])
        .arg(path)
        .arg(mode)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Applies `mode` through a temporary image and returns the modified `disk`.
fn edited(mut disk: Disk, mode: &str) -> Disk {
    let path = std::env::temp_dir().join(format!("gpt-native-edit-{}.img", std::process::id()));
    fs::write(&path, &disk.bytes).unwrap();
    edit_table(&path, mode);
    disk.bytes = fs::read(&path).unwrap();
    fs::remove_file(path).unwrap();
    disk.operations = 0;
    disk
}

/// Checks interrupted growth, identity retention, conflict refusal and repeated
/// no-op behavior through public GPT APIs, with an independent sfdisk readback.
#[test]
fn growth_survives_each_interrupted_write_and_keeps_partition_identity() {
    let original = fixture();
    let mut complete = original.clone();
    let identity = scan_volumes(&mut complete, DiskId(0)).unwrap().volumes[0].clone();
    let target = grow_partition(&mut complete, PartitionId(1)).unwrap();
    assert_eq!(
        &complete.bytes[512 + 56..512 + 72],
        &original.bytes[512 + 56..512 + 72]
    );
    assert_eq!(
        &complete.bytes[1024 + 48..1024 + 56],
        &original.bytes[1024 + 48..1024 + 56]
    );
    assert_eq!(
        &complete.bytes[1024 + 128..1024 + 256],
        &original.bytes[1024 + 128..1024 + 256]
    );
    let operations = complete.operations;
    assert!(operations > 0);
    // GPT CRCs do not cover the PMBR. A crash can leave both new GPT copies
    // durable while sector zero still protects only the old image capacity.
    let mut stale_mbr = complete.clone();
    stale_mbr.bytes[..512].copy_from_slice(&original.bytes[..512]);
    stale_mbr.operations = 0;
    assert_eq!(
        grow_partition(&mut stale_mbr, PartitionId(1)).unwrap(),
        target
    );
    assert!(
        stale_mbr.bytes == complete.bytes,
        "must repair a stale PMBR"
    );
    assert_eq!(stale_mbr.operations, 2, "repair only PMBR and flush");
    grow_partition(&mut stale_mbr, PartitionId(1)).unwrap();
    assert_eq!(stale_mbr.operations, 2, "repaired layout must be a no-op");
    assert_eq!(target.start_block, 2048);
    assert_eq!(
        target.start_block + target.num_blocks,
        complete.num_blocks() - 65
    );
    for source in [original.clone(), edited(original.clone(), "stale-tail")] {
        let mut converged = source.clone();
        assert_eq!(
            grow_partition(&mut converged, PartitionId(1)).unwrap(),
            target
        );
        assert_eq!(converged.bytes, complete.bytes);
        for interruption in 1..=operations {
            let mut disk = source.clone();
            disk.fail_at = Some(interruption);
            assert!(grow_partition(&mut disk, PartitionId(1)).is_err());
            disk.fail_at = None;
            let recovered = scan_volumes(&mut disk, DiskId(0)).unwrap().volumes[0].clone();
            assert_eq!(
                recovered.partuuid, identity.partuuid,
                "interruption {interruption}"
            );
            assert_eq!(recovered.partlabel, identity.partlabel);
            assert_eq!(grow_partition(&mut disk, PartitionId(1)).unwrap(), target);
            disk.operations = 0;
            assert_eq!(grow_partition(&mut disk, PartitionId(1)).unwrap(), target);
            assert_eq!(disk.operations, 0, "repeated growth must not write");
        }
    }
    // A compatible new physical-tail backup is authoritative after its commit,
    // even while the primary still matches the old image-end backup.
    let mut pending_primary = complete.clone();
    pending_primary.bytes[..2048 * 512].copy_from_slice(&original.bytes[..2048 * 512]);
    assert_eq!(
        partition_growth(&mut pending_primary, PartitionId(1))
            .unwrap()
            .current,
        target
    );
    let path = std::env::temp_dir().join(format!("gpt-native-verify-{}.img", std::process::id()));
    fs::write(&path, &complete.bytes).unwrap();
    let output = Command::new("sfdisk")
        .arg("--verify")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(path).unwrap();
    // A corrupt primary and a sole physical-tail backup require restoring
    // redundancy before that same backup location can be rewritten.
    let mut sole_backup = edited(complete.clone(), "small-root");
    sole_backup.bytes[512 + 16] ^= 1;
    let mut recovered = sole_backup.clone();
    assert_eq!(
        grow_partition(&mut recovered, PartitionId(1)).unwrap(),
        target
    );
    for cut in 1..=recovered.operations {
        let mut disk = sole_backup.clone();
        disk.fail_at = Some(cut);
        assert!(grow_partition(&mut disk, PartitionId(1)).is_err());
        disk.fail_at = None;
        assert_eq!(
            scan_volumes(&mut disk, DiskId(0)).unwrap().volumes[0].partuuid,
            identity.partuuid
        );
        assert_eq!(grow_partition(&mut disk, PartitionId(1)).unwrap(), target);
    }
    let mut conflict = edited(complete.clone(), "conflict");
    assert_eq!(
        grow_partition(&mut conflict, PartitionId(1)).unwrap(),
        target
    );
    assert_eq!(conflict.bytes, complete.bytes);
    // A valid primary remains authoritative even without a matching backup.
    let mut damaged_backup = edited(original.clone(), "stale-tail");
    let old_backup =
        u64::from_le_bytes(damaged_backup.bytes[512 + 32..512 + 40].try_into().unwrap());
    damaged_backup.bytes[old_backup as usize * 512 + 16] ^= 1;
    assert_eq!(
        grow_partition(&mut damaged_backup, PartitionId(1)).unwrap(),
        target
    );
    let mut expected = complete.bytes.clone();
    // The retired image-end header is now partition data, so growth must not
    // erase or repair it while relocating the authoritative backup.
    expected[old_backup as usize * 512 + 16] ^= 1;
    assert_eq!(damaged_backup.bytes, expected);
    // Without a valid primary, unrelated valid backups have no authority or
    // generation ordering; refuse rather than choosing one and overwriting it.
    let mut ambiguous = edited(original.clone(), "stale-tail");
    ambiguous.bytes[512 + 16] ^= 1;
    assert_eq!(
        grow_partition(&mut ambiguous, PartitionId(1)),
        Err(Error::ConflictingPartitionTables)
    );
    assert_eq!(ambiguous.operations, 0);
    let mut missing_pmbr = original.clone();
    missing_pmbr.bytes[446 + 4] = 0;
    assert!(grow_partition(&mut missing_pmbr, PartitionId(1)).is_err());
    assert_eq!(missing_pmbr.operations, 0);
    // Redundant reads tolerate one unreadable copy; mutation refuses that uncertainty.
    for unreadable in [1, complete.num_blocks() - 1] {
        let mut disk = complete.clone();
        disk.operations = 0;
        disk.bad_read = Some(unreadable);
        assert_eq!(
            scan_volumes(&mut disk, DiskId(0)).unwrap().volumes[0].partuuid,
            identity.partuuid
        );
        assert!(grow_partition(&mut disk, PartitionId(1)).is_err());
        assert_eq!(disk.operations, 0);
    }
}
