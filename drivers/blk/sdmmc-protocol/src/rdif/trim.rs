//! Checked eMMC TRIM capability and IRQ-driven non-data command sequence.

use rdif_block::BlkError;

use crate::{
    Command,
    ext_csd::ExtCsd,
    response::{CardState, CidResponse, R1Response, ResponseType},
};

const TRIM_BUSY_BUDGET_MS: u32 = 20_000;
// Linux v6.12 R1_STATUS, including WP_ERASE_SKIP and ERASE_RESET, plus
// SWITCH_ERROR. Generic R1 parsing alone omits these lower status bits.
const TRIM_STATUS_ERROR_MASK: u32 = 0xFFF9_A080;

/// Standard TRIM facts checked before a CQHCI queue publishes WriteZeroes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmcTrimSupport {
    /// User-area capacity in sector-addressed 512-byte blocks.
    capacity_blocks: u32,
    /// Active HC erase-group size in 512-byte blocks.
    erase_group_blocks: u32,
    /// Declared TRIM busy duration for one touched erase group, in ms.
    group_timeout_ms: u32,
    /// Largest request that fits the busy budget at any group offset.
    max_blocks: u32,
}

/// TRIM progress owned by the queue task until success or terminal failure.
///
/// The caller drains all preceding data tasks, excludes subsequent commands,
/// and advances this state only after a confirmed completion IRQ. Every
/// command uses CQHCI's QBR and direct-command slot. Lost IRQs still require
/// the caller's request watchdog; this state does not manage timers.
#[derive(Debug)]
pub struct MmcTrimRequest {
    /// Inclusive first sector address; never rounded to erase geometry.
    start: u32,
    /// Inclusive final sector address, checked against user-area capacity.
    end: u32,
    /// Selected card address used by ordinary CMD13, with SQS left clear.
    rca: u16,
    /// Busy duration in ns derived from this range's touched erase groups.
    timeout_ns: u64,
    /// Fixed monotonic deadline in ns, armed immediately before CMD38.
    /// Before that point, the owning runtime's request watchdog bounds waits.
    deadline_ns: Option<u64>,
    /// Sole queue-task-owned command stage; terminal states cannot advance.
    phase: TrimPhase,
}

#[derive(Clone, Copy, Debug)]
enum TrimPhase {
    Before,
    Start,
    End,
    Trim,
    Status,
    Terminal,
}

impl MmcTrimSupport {
    /// Checks sector-addressed, CQ-capable MMC facts and Linux's known TRIM
    /// exceptions. Unknown private firmware defects remain a device contract
    /// limitation; missing or malformed identity never enables the operation.
    pub fn for_card(ext_csd: &ExtCsd, cid: &CidResponse, high_capacity: bool) -> Option<Self> {
        if !high_capacity
            || !ext_csd.command_queue_supported()
            || !ext_csd.supports_trim()
            || ext_csd.erased_byte() != Some(0)
            || !standard_trim_identity(cid)
        {
            return None;
        }
        let capacity_blocks = ext_csd.sector_count()?;
        let erase_group_blocks = ext_csd.hc_erase_group_blocks()?;
        let group_timeout_ms = ext_csd.trim_timeout_ms()?;
        let groups = TRIM_BUSY_BUDGET_MS / group_timeout_ms;
        if groups == 0 {
            return None;
        }
        // At an arbitrary starting offset, N blocks touch at most
        // floor((N + group_size - 2) / group_size) + 1 groups.
        let max_blocks = (groups - 1)
            .checked_mul(erase_group_blocks)?
            .checked_add(1)?
            .min(capacity_blocks);
        Some(Self {
            capacity_blocks,
            erase_group_blocks,
            group_timeout_ms,
            max_blocks,
        })
    }

    /// Maximum 512-byte block count safe at any starting group offset.
    pub const fn max_blocks(self) -> u32 {
        self.max_blocks
    }

    /// Validates the exact requested range without erase-group rounding.
    /// Invalid lengths, card addresses, capacity, or RCA return an error.
    pub fn request(self, lba: u64, count: u32, rca: u16) -> Result<MmcTrimRequest, BlkError> {
        if count == 0 || count > self.max_blocks || rca == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let end = lba
            .checked_add(u64::from(count - 1))
            .filter(|end| *end < u64::from(self.capacity_blocks))
            .ok_or(BlkError::InvalidRequest)?;
        let start = u32::try_from(lba).map_err(|_| BlkError::InvalidRequest)?;
        let end = u32::try_from(end).map_err(|_| BlkError::InvalidRequest)?;
        let groups = end / self.erase_group_blocks - start / self.erase_group_blocks + 1;
        let timeout_ns = u64::from(self.group_timeout_ms) * u64::from(groups) * 1_000_000;
        Ok(MmcTrimRequest {
            start,
            end,
            rca,
            timeout_ns,
            deadline_ns: None,
            phase: TrimPhase::Before,
        })
    }
}

impl MmcTrimRequest {
    /// Returns the next command, initially or after `advance` returns false.
    /// CMD38 intentionally uses R1; subsequent CMD13 responses prove readiness
    /// without relying on the host's hardware busy timeout.
    /// After success or error the caller must discard this request, never
    /// submit another command from it or retry the partially executed range.
    pub fn command(&self) -> Command {
        let (index, argument) = match self.phase {
            TrimPhase::Start => (35, self.start),
            TrimPhase::End => (36, self.end),
            TrimPhase::Trim => (38, 1),
            _ => (13, u32::from(self.rca) << 16),
        };
        Command::new(index, argument, ResponseType::R1)
    }

    /// Consumes one confirmed command response at a monotonic time in ns.
    /// Returns true only after the final status proves ready+TRAN. The
    /// CMD36 response fixes the busy deadline immediately before CMD38 is
    /// published; status retries cannot extend it. Any error is terminal.
    pub fn advance(&mut self, raw_r1: u32, now_ns: u64) -> Result<bool, BlkError> {
        let phase = core::mem::replace(&mut self.phase, TrimPhase::Terminal);
        if matches!(phase, TrimPhase::Terminal) {
            return Err(BlkError::InvalidRequest);
        }
        if raw_r1 & TRIM_STATUS_ERROR_MASK != 0 {
            return Err(BlkError::Io);
        }
        let status = R1Response::from_native_raw(raw_r1).map_err(|_| BlkError::Io)?;
        if self.deadline_ns.is_some_and(|deadline| now_ns >= deadline) {
            return Err(BlkError::TimedOut);
        }
        let ready = status.ready_for_data() && status.current_state() == CardState::Transfer;
        let may_be_busy = matches!(
            status.current_state(),
            CardState::Programming | CardState::Transfer
        );
        self.phase = match phase {
            TrimPhase::Before => {
                if ready {
                    TrimPhase::Start
                } else if may_be_busy {
                    // Completing the preceding data task can race with the
                    // card's final programming busy interval. Keep querying;
                    // the request watchdog bounds this pre-TRIM wait.
                    TrimPhase::Before
                } else {
                    return Err(BlkError::Io);
                }
            }
            TrimPhase::Start => TrimPhase::End,
            TrimPhase::End => {
                self.deadline_ns = Some(
                    now_ns
                        .checked_add(self.timeout_ns)
                        .ok_or(BlkError::TimedOut)?,
                );
                TrimPhase::Trim
            }
            TrimPhase::Trim => TrimPhase::Status,
            TrimPhase::Status => {
                if ready {
                    return Ok(true);
                }
                if !may_be_busy {
                    return Err(BlkError::Io);
                }
                TrimPhase::Status
            }
            TrimPhase::Terminal => return Err(BlkError::InvalidRequest),
        };
        Ok(false)
    }
}

fn standard_trim_identity(cid: &CidResponse) -> bool {
    let manufacturer = cid.raw[0];
    // Linux's modern MMC fixups match the full legacy OEM field, including
    // the byte now containing CBX/reserved bits, at CID bits 119:104.
    let oem = u16::from_be_bytes([cid.raw[1], cid.raw[2]]);
    let product = &cid.raw[3..9];
    if matches!(manufacturer, 0 | 0xff)
        || oem == u16::MAX
        || !product.iter().all(|byte| (0x20..=0x7e).contains(byte))
        || product.iter().all(|byte| *byte == b' ')
    {
        return false;
    }
    // Linux v6.12 drivers/mmc/core/quirks.h: INAND_CMD38 and TRIM_BROKEN.
    !matches!(
        (manufacturer, oem, product),
        (
            0x45,
            0x0100,
            b"SEM02G" | b"SEM04G" | b"SEM08G" | b"SEM16G" | b"SEM32G"
        ) | (0x70, _, b"V10008" | b"V10016")
            | (0x13, 0x014e, b"Q2J54A")
            | (0x70, 0x0100, b"M62704")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: u32 = (4 << 9) | (1 << 8);
    const PROGRAMMING: u32 = 7 << 9;

    fn card(multiplier: u8) -> (ExtCsd, CidResponse) {
        let mut ext = [0; 512];
        ext[192] = 8;
        ext[308] = 1;
        ext[307] = 30;
        ext[175] = 1;
        ext[224] = 1;
        ext[231] = 1 << 4;
        ext[232] = multiplier;
        ext[212..216].copy_from_slice(&1_000_000_u32.to_le_bytes());
        let mut cid = [0; 16];
        cid[0] = 0x15;
        cid[1..3].copy_from_slice(&0x0100_u16.to_be_bytes());
        cid[3..9].copy_from_slice(b"ABCDEF");
        (ExtCsd::from_bytes(ext), CidResponse::from_raw(cid))
    }

    #[test]
    fn trim_sequence_preserves_exact_range_and_waits_for_ready() {
        let (ext, cid) = card(1);
        let support = MmcTrimSupport::for_card(&ext, &cid, true).unwrap();
        // This unaligned two-block range touches two erase groups.
        let mut request = support.request(1023, 2, 7).unwrap();
        for busy in [PROGRAMMING, 4 << 9] {
            assert!(!request.advance(busy, 0).unwrap());
            assert_eq!(request.command().index, 13);
        }
        for (index, argument) in [(13, 7 << 16), (35, 1023), (36, 1024), (38, 1)] {
            let next = request.command();
            assert_eq!(next.index, index);
            assert_eq!(next.argument, argument);
            assert_eq!(next.response, ResponseType::R1);
            assert!(!request.advance(READY, 100).unwrap());
        }
        let status = request.command();
        assert_eq!(status.index, 13);
        assert!(!request.advance(PROGRAMMING, 500_000_100).unwrap());
        assert!(request.advance(READY, 599_000_100).unwrap());
        assert_eq!(
            request.advance(READY, 599_000_100),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn trim_rejects_errors_and_keeps_one_busy_deadline() {
        let (ext, cid) = card(1);
        let support = MmcTrimSupport::for_card(&ext, &cid, true).unwrap();
        for failed_stage in 0..5 {
            for error in [1 << 31, 1 << 26, 1 << 19, 1 << 15, 1 << 13, 1 << 7] {
                let mut request = support.request(1023, 2, 1).unwrap();
                for _ in 0..failed_stage {
                    assert!(!request.advance(READY, 0).unwrap());
                }
                assert_eq!(request.advance(READY | error, 0), Err(BlkError::Io));
                assert_eq!(request.advance(READY, 0), Err(BlkError::InvalidRequest));
            }
        }
        let mut request = support.request(1023, 2, 1).unwrap();
        for now_ns in [0, 0, 100, 300_000_100] {
            assert!(!request.advance(READY, now_ns).unwrap());
        }
        assert!(!request.advance(PROGRAMMING, 500_000_100).unwrap());
        assert_eq!(
            request.advance(PROGRAMMING, 600_000_100),
            Err(BlkError::TimedOut)
        );

        // An unrelated state before CMD35 must never start erase selection.
        let mut request = support.request(0, 1, 1).unwrap();
        assert_eq!(request.advance(3 << 9, 0), Err(BlkError::Io));
    }

    #[test]
    fn trim_capability_bounds_unaligned_ranges_and_rejects_invalid_cards() {
        let (ext, cid) = card(1);
        let support = MmcTrimSupport::for_card(&ext, &cid, true).unwrap();
        let maximum = support.max_blocks();
        assert_eq!(maximum, 65 * 1024 + 1);
        assert!(support.request(1023, maximum, 1).is_ok());
        assert!(support.request(1023, maximum + 1, 1).is_err());
        assert!(support.request(0, 0, 1).is_err());
        assert!(support.request(u64::MAX, 2, 1).is_err());
        assert!(support.request(999_999, 2, 1).is_err());
        assert!(support.request(0, 1, 0).is_err());
        assert!(MmcTrimSupport::for_card(&ext, &cid, false).is_none());
        for (offset, value) in [(175, 0), (224, 0), (231, 0), (232, 0), (181, 1)] {
            let mut raw = *ext.as_bytes();
            raw[offset] = value;
            assert!(MmcTrimSupport::for_card(&ExtCsd::from_bytes(raw), &cid, true).is_none());
        }
        let (slow, _) = card(67);
        assert!(MmcTrimSupport::for_card(&slow, &cid, true).is_none());
        let (one_group, _) = card(66);
        let one_group = MmcTrimSupport::for_card(&one_group, &cid, true).unwrap();
        assert_eq!(one_group.max_blocks(), 1);
        assert!(one_group.request(1023, 1, 1).is_ok());
        assert!(one_group.request(1023, 2, 1).is_err());
        assert!(MmcTrimSupport::for_card(&ext, &CidResponse::from_raw([0; 16]), true).is_none());

        // Linux's exception matches must use the six-byte MMC product name
        // and its full legacy 16-bit OEM match, not the SD five-byte name.
        for (manufacturer, oem, product) in [
            (0x45, 0x0100_u16, *b"SEM16G"),
            (0x70, 0x0000, *b"V10008"),
            (0x13, 0x014e, *b"Q2J54A"),
            (0x70, 0x0100, *b"M62704"),
        ] {
            let mut raw = cid.raw;
            raw[0] = manufacturer;
            raw[1..3].copy_from_slice(&oem.to_be_bytes());
            raw[3..9].copy_from_slice(&product);
            assert!(MmcTrimSupport::for_card(&ext, &CidResponse::from_raw(raw), true).is_none());
        }
    }
}
