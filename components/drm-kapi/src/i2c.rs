// SPDX-License-Identifier: MIT
//! I2C adapter model for the DRM ports: adapter registration against a number
//! registry, batched master transfers with quirk validation, and retries for
//! `-EAGAIN` results within a time window.
//!
//! Device enumeration, sysfs exposure and the SMBus emulation layer are not
//! modelled here; the platform owns the device model.

use alloc::vec::Vec;

use crate::{
    clock,
    config::{CONFIG_HZ, EAGAIN, EBUSY, EINVAL, EOPNOTSUPP},
};

// Per-message flags.
pub const I2C_M_RD: u16 = 0x0001;
pub const I2C_M_TEN: u16 = 0x0010;
pub const I2C_M_DMA_SAFE: u16 = 0x0200;
pub const I2C_M_RECV_LEN: u16 = 0x0400;
pub const I2C_M_NO_RD_ACK: u16 = 0x0800;
pub const I2C_M_IGNORE_NAK: u16 = 0x1000;
pub const I2C_M_REV_DIR_ADDR: u16 = 0x2000;
pub const I2C_M_NOSTART: u16 = 0x4000;
pub const I2C_M_STOP: u16 = 0x8000;

// Adapter capability bits reported by `I2cAlgorithm::functionality`.
pub const I2C_FUNC_I2C: u32 = 0x0000_0001;
pub const I2C_FUNC_10BIT_ADDR: u32 = 0x0000_0002;
pub const I2C_FUNC_PROTOCOL_MANGLING: u32 = 0x0000_0004;
pub const I2C_FUNC_SMBUS_PEC: u32 = 0x0000_0008;
pub const I2C_FUNC_NOSTART: u32 = 0x0000_0010;
pub const I2C_FUNC_SLAVE: u32 = 0x0000_0020;
pub const I2C_FUNC_SMBUS_BLOCK_PROC_CALL: u32 = 0x0000_8000;
pub const I2C_FUNC_SMBUS_QUICK: u32 = 0x0001_0000;
pub const I2C_FUNC_SMBUS_READ_BYTE: u32 = 0x0002_0000;
pub const I2C_FUNC_SMBUS_WRITE_BYTE: u32 = 0x0004_0000;
pub const I2C_FUNC_SMBUS_READ_BYTE_DATA: u32 = 0x0008_0000;
pub const I2C_FUNC_SMBUS_WRITE_BYTE_DATA: u32 = 0x0010_0000;
pub const I2C_FUNC_SMBUS_READ_WORD_DATA: u32 = 0x0020_0000;
pub const I2C_FUNC_SMBUS_WRITE_WORD_DATA: u32 = 0x0040_0000;
pub const I2C_FUNC_SMBUS_PROC_CALL: u32 = 0x0080_0000;
pub const I2C_FUNC_SMBUS_READ_BLOCK_DATA: u32 = 0x0100_0000;
pub const I2C_FUNC_SMBUS_WRITE_BLOCK_DATA: u32 = 0x0200_0000;
pub const I2C_FUNC_SMBUS_READ_I2C_BLOCK: u32 = 0x0400_0000;
pub const I2C_FUNC_SMBUS_WRITE_I2C_BLOCK: u32 = 0x0800_0000;
pub const I2C_FUNC_SMBUS_HOST_NOTIFY: u32 = 0x1000_0000;
pub const I2C_FUNC_SMBUS_BYTE: u32 = I2C_FUNC_SMBUS_READ_BYTE | I2C_FUNC_SMBUS_WRITE_BYTE;
pub const I2C_FUNC_SMBUS_BYTE_DATA: u32 =
    I2C_FUNC_SMBUS_READ_BYTE_DATA | I2C_FUNC_SMBUS_WRITE_BYTE_DATA;
pub const I2C_FUNC_SMBUS_WORD_DATA: u32 =
    I2C_FUNC_SMBUS_READ_WORD_DATA | I2C_FUNC_SMBUS_WRITE_WORD_DATA;
pub const I2C_FUNC_SMBUS_BLOCK_DATA: u32 =
    I2C_FUNC_SMBUS_READ_BLOCK_DATA | I2C_FUNC_SMBUS_WRITE_BLOCK_DATA;
pub const I2C_FUNC_SMBUS_I2C_BLOCK: u32 =
    I2C_FUNC_SMBUS_READ_I2C_BLOCK | I2C_FUNC_SMBUS_WRITE_I2C_BLOCK;
pub const I2C_FUNC_SMBUS_EMUL: u32 = I2C_FUNC_SMBUS_QUICK
    | I2C_FUNC_SMBUS_BYTE
    | I2C_FUNC_SMBUS_BYTE_DATA
    | I2C_FUNC_SMBUS_WORD_DATA
    | I2C_FUNC_SMBUS_PROC_CALL
    | I2C_FUNC_SMBUS_WRITE_BLOCK_DATA
    | I2C_FUNC_SMBUS_I2C_BLOCK
    | I2C_FUNC_SMBUS_PEC;
pub const I2C_FUNC_SMBUS_EMUL_ALL: u32 =
    I2C_FUNC_SMBUS_EMUL | I2C_FUNC_SMBUS_READ_BLOCK_DATA | I2C_FUNC_SMBUS_BLOCK_PROC_CALL;

/// Largest SMBus block payload, in bytes.
pub const I2C_SMBUS_BLOCK_MAX: usize = 32;

// Adapter quirk bits carried in `I2cAdapterQuirks::flags`.
pub const I2C_AQ_COMB: u64 = 1 << 0;
pub const I2C_AQ_COMB_WRITE_FIRST: u64 = 1 << 1;
pub const I2C_AQ_COMB_READ_SECOND: u64 = 1 << 2;
pub const I2C_AQ_COMB_SAME_ADDR: u64 = 1 << 3;
pub const I2C_AQ_COMB_WRITE_THEN_READ: u64 =
    I2C_AQ_COMB | I2C_AQ_COMB_WRITE_FIRST | I2C_AQ_COMB_READ_SECOND | I2C_AQ_COMB_SAME_ADDR;
pub const I2C_AQ_NO_CLK_STRETCH: u64 = 1 << 4;
pub const I2C_AQ_NO_ZERO_LEN_READ: u64 = 1 << 5;
pub const I2C_AQ_NO_ZERO_LEN_WRITE: u64 = 1 << 6;
pub const I2C_AQ_NO_ZERO_LEN: u64 = I2C_AQ_NO_ZERO_LEN_READ | I2C_AQ_NO_ZERO_LEN_WRITE;
pub const I2C_AQ_NO_REP_START: u64 = 1 << 7;

/// One message of a transfer. `len` is the byte count used on the bus and
/// never exceeds `buf.len()`.
pub struct I2cMsg<'a> {
    pub addr: u16,
    pub flags: u16,
    pub len: usize,
    pub buf: &'a mut [u8],
}

impl<'a> I2cMsg<'a> {
    /// A message covering the whole of `buf`.
    pub fn new(addr: u16, flags: u16, buf: &'a mut [u8]) -> Self {
        Self {
            addr,
            flags,
            len: buf.len(),
            buf,
        }
    }
}

/// Limits an adapter places on the batches it accepts. A zero limit means
/// "unlimited".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct I2cAdapterQuirks {
    pub flags: u64,
    pub max_num_msgs: i32,
    pub max_write_len: u16,
    pub max_read_len: u16,
    pub max_comb_1st_msg_len: u16,
    pub max_comb_2nd_msg_len: u16,
}

/// Bus-master operations supplied by a controller driver.
pub trait I2cAlgorithm: Sync {
    /// Executes the batch. Returns the number of messages completed, or a
    /// negative errno.
    fn master_xfer(&self, adap: &I2cAdapter<'_>, msgs: &mut [I2cMsg<'_>]) -> i32;

    /// Bit set of `I2C_FUNC_*` capabilities this controller offers.
    fn functionality(&self) -> u32;
}

/// A bus adapter. Whole transfers are serialised by an internal lock.
pub struct I2cAdapter<'a> {
    pub name: &'a str,
    /// Bus number. `-1` asks for a dynamically chosen number.
    pub nr: i32,
    pub algo: Option<&'a dyn I2cAlgorithm>,
    pub quirks: Option<&'a I2cAdapterQuirks>,
    /// Additional attempts after an `-EAGAIN` result, bounded by `timeout`.
    pub retries: u32,
    /// Retry window in ticks. Zero is replaced by one second at registration.
    pub timeout: i64,
    bus_lock: spin::Mutex<()>,
}

impl<'a> I2cAdapter<'a> {
    pub fn new(name: &'a str, algo: Option<&'a dyn I2cAlgorithm>) -> Self {
        Self {
            name,
            nr: -1,
            algo,
            quirks: None,
            retries: 0,
            timeout: 0,
            bus_lock: spin::Mutex::new(()),
        }
    }

    /// Runs `msgs` as one exclusive transfer on this adapter.
    pub fn transfer(&self, msgs: &mut [I2cMsg<'_>]) -> i32 {
        let _exclusive = self.bus_lock.lock();
        self.run_batch(msgs)
    }

    /// Validates and dispatches `msgs`. The caller holds the bus lock.
    fn run_batch(&self, msgs: &mut [I2cMsg<'_>]) -> i32 {
        let Some(algo) = self.algo else {
            log::debug!("I2C level transfers not supported");
            return -EOPNOTSUPP;
        };
        if msgs.is_empty() {
            return -EINVAL;
        }
        if let Some(quirks) = self.quirks
            && rejected_by_quirks(self, quirks, msgs)
        {
            return -EOPNOTSUPP;
        }

        let started = clock::now_ns();
        let window_ns = self.retry_window_ns();
        let mut retries_left = self.retries;
        loop {
            let ret = algo.master_xfer(self, msgs);
            let out_of_time = clock::now_ns() > started.saturating_add(window_ns);
            if ret != -EAGAIN || retries_left == 0 || out_of_time {
                return ret;
            }
            retries_left -= 1;
        }
    }

    /// The retry window converted from ticks to nanoseconds.
    fn retry_window_ns(&self) -> u64 {
        let ns_per_tick = 1_000_000_000 / u64::from(CONFIG_HZ);
        u64::try_from(self.timeout)
            .unwrap_or(0)
            .saturating_mul(ns_per_tick)
    }
}

/// Logs why a batch was refused and reports that it was.
fn refuse(adap: &I2cAdapter<'_>, msg: &I2cMsg<'_>, reason: &str) -> bool {
    let direction = if msg.flags & I2C_M_RD != 0 {
        "read"
    } else {
        "write"
    };
    log::warn!(
        "{}: adapter quirk: {} (addr 0x{:04x}, size {}, {})",
        adap.name,
        reason,
        msg.addr,
        msg.len,
        direction
    );
    true
}

/// True when `limit` is set and `value` exceeds it.
fn over_limit(value: usize, limit: usize) -> bool {
    limit != 0 && value > limit
}

/// Checks `msgs` against `q`. Returns true when the batch must be refused.
fn rejected_by_quirks(adap: &I2cAdapter<'_>, q: &I2cAdapterQuirks, msgs: &[I2cMsg<'_>]) -> bool {
    let mut max_msgs = usize::try_from(q.max_num_msgs).unwrap_or(0);
    let mut check_lengths = true;

    // A combined pair is a single write-then-read exchange with its own
    // rules, and its per-message limits replace the generic ones.
    if q.flags & I2C_AQ_COMB != 0 {
        max_msgs = 2;
        if let [first, second] = msgs {
            if q.flags & I2C_AQ_COMB_WRITE_FIRST != 0 && first.flags & I2C_M_RD != 0 {
                return refuse(adap, first, "1st comb msg must be write");
            }
            if q.flags & I2C_AQ_COMB_READ_SECOND != 0 && second.flags & I2C_M_RD == 0 {
                return refuse(adap, second, "2nd comb msg must be read");
            }
            if q.flags & I2C_AQ_COMB_SAME_ADDR != 0 && first.addr != second.addr {
                return refuse(adap, first, "comb msg only to same addr");
            }
            if over_limit(first.len, usize::from(q.max_comb_1st_msg_len)) {
                return refuse(adap, first, "msg too long");
            }
            if over_limit(second.len, usize::from(q.max_comb_2nd_msg_len)) {
                return refuse(adap, second, "msg too long");
            }
            check_lengths = false;
        }
    }

    if over_limit(msgs.len(), max_msgs) {
        return refuse(adap, &msgs[0], "too many messages");
    }

    for msg in msgs {
        let (max_len, no_zero_len) = if msg.flags & I2C_M_RD != 0 {
            (q.max_read_len, q.flags & I2C_AQ_NO_ZERO_LEN_READ != 0)
        } else {
            (q.max_write_len, q.flags & I2C_AQ_NO_ZERO_LEN_WRITE != 0)
        };
        if check_lengths && over_limit(msg.len, usize::from(max_len)) {
            return refuse(adap, msg, "msg too long");
        }
        if no_zero_len && msg.len == 0 {
            return refuse(adap, msg, "no zero length");
        }
    }
    false
}

/// Registry of bus numbers in use.
pub struct I2cAdapterIdr {
    numbers: spin::Mutex<Vec<i32>>,
}

impl I2cAdapterIdr {
    pub const fn new() -> Self {
        Self {
            numbers: spin::Mutex::new(Vec::new()),
        }
    }

    /// Claims the lowest unused number, starting from zero.
    fn alloc_dynamic(&self) -> i32 {
        let mut numbers = self.numbers.lock();
        let free = (0..)
            .find(|candidate| !numbers.contains(candidate))
            .unwrap_or(0);
        numbers.push(free);
        free
    }

    /// Claims `nr`, or fails with `-EBUSY` if it is already in use.
    fn alloc_fixed(&self, nr: i32) -> Result<(), i32> {
        let mut numbers = self.numbers.lock();
        if numbers.contains(&nr) {
            return Err(-EBUSY);
        }
        numbers.push(nr);
        Ok(())
    }

    fn release(&self, nr: i32) {
        self.numbers.lock().retain(|&claimed| claimed != nr);
    }

    pub fn contains(&self, nr: i32) -> bool {
        self.numbers.lock().contains(&nr)
    }
}

impl Default for I2cAdapterIdr {
    fn default() -> Self {
        Self::new()
    }
}

/// The system-wide bus number registry.
pub static I2C_ADAPTER_IDR: I2cAdapterIdr = I2cAdapterIdr::new();

/// Validates the adapter, fills in its defaults and claims its number. Returns
/// zero on success or a negative errno.
fn register_adapter(adap: &mut I2cAdapter<'_>, idr: &I2cAdapterIdr) -> i32 {
    if adap.name.is_empty() {
        log::warn!("i2c adapter has no name");
        return -EINVAL;
    }
    if adap.algo.is_none() {
        log::error!("adapter '{}': no algo supplied!", adap.name);
        return -EINVAL;
    }
    if adap.timeout == 0 {
        adap.timeout = i64::from(CONFIG_HZ);
    }
    if adap.nr == -1 {
        adap.nr = idr.alloc_dynamic();
        return 0;
    }
    match idr.alloc_fixed(adap.nr) {
        Ok(()) => 0,
        Err(err) => {
            log::error!("adapter '{}': bus number {} busy", adap.name, adap.nr);
            err
        }
    }
}

/// Registers `adap` under the next free number. The number is restored to
/// `-1` if registration fails.
pub fn i2c_add_adapter(adap: &mut I2cAdapter<'_>, idr: &I2cAdapterIdr) -> i32 {
    let requested = adap.nr;
    adap.nr = -1;
    let ret = register_adapter(adap, idr);
    if ret != 0 {
        adap.nr = requested;
    }
    ret
}

/// Registers `adap`. A number of `-1` is assigned dynamically; any other value
/// is claimed as-is.
pub fn i2c_add_numbered_adapter(adap: &mut I2cAdapter<'_>, idr: &I2cAdapterIdr) -> i32 {
    if adap.nr == -1 {
        return i2c_add_adapter(adap, idr);
    }
    register_adapter(adap, idr)
}

/// Releases the bus number held by `adap`.
pub fn i2c_del_adapter(adap: &mut I2cAdapter<'_>, idr: &I2cAdapterIdr) {
    idr.release(adap.nr);
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Replays scripted results: entry `n` answers call `n`, and the last
    /// entry repeats. Negative answers are returned as-is, non-negative ones
    /// report every message as done.
    struct Scripted {
        calls: AtomicU32,
        script: [i32; 4],
    }

    impl I2cAlgorithm for Scripted {
        fn master_xfer(&self, _adap: &I2cAdapter<'_>, msgs: &mut [I2cMsg<'_>]) -> i32 {
            let index = self.calls.fetch_add(1, Ordering::Relaxed) as usize;
            let answer = self.script[index.min(self.script.len() - 1)];
            if answer < 0 {
                answer
            } else {
                msgs.len() as i32
            }
        }

        fn functionality(&self) -> u32 {
            I2C_FUNC_I2C
        }
    }

    fn scripted(script: [i32; 4]) -> Scripted {
        Scripted {
            calls: AtomicU32::new(0),
            script,
        }
    }

    #[test]
    fn dynamic_and_fixed_numbers_follow_the_idr_rules() {
        let idr = I2cAdapterIdr::new();
        let algo = scripted([0; 4]);
        let mut first = I2cAdapter::new("a", Some(&algo));
        assert_eq!(i2c_add_adapter(&mut first, &idr), 0);
        assert_eq!(first.nr, 0);
        let mut second = I2cAdapter::new("b", Some(&algo));
        assert_eq!(i2c_add_numbered_adapter(&mut second, &idr), 0);
        assert_eq!(second.nr, 1);
        let mut fixed = I2cAdapter::new("c", Some(&algo));
        fixed.nr = 5;
        assert_eq!(i2c_add_numbered_adapter(&mut fixed, &idr), 0);
        let mut clash = I2cAdapter::new("d", Some(&algo));
        clash.nr = 5;
        assert_eq!(i2c_add_numbered_adapter(&mut clash, &idr), -EBUSY);
        i2c_del_adapter(&mut fixed, &idr);
        assert!(!idr.contains(5));
        let mut reused = I2cAdapter::new("e", Some(&algo));
        reused.nr = 5;
        assert_eq!(i2c_add_numbered_adapter(&mut reused, &idr), 0);
    }

    #[test]
    fn registration_rejects_nameless_or_algorithmless_adapters_and_defaults_timeout() {
        let idr = I2cAdapterIdr::new();
        let algo = scripted([0; 4]);
        let mut nameless = I2cAdapter::new("", Some(&algo));
        assert_eq!(i2c_add_adapter(&mut nameless, &idr), -EINVAL);
        let mut bare = I2cAdapter::new("bare", None);
        assert_eq!(i2c_add_adapter(&mut bare, &idr), -EINVAL);
        let mut ok = I2cAdapter::new("ok", Some(&algo));
        assert_eq!(i2c_add_adapter(&mut ok, &idr), 0);
        assert_eq!(ok.timeout, i64::from(CONFIG_HZ));
    }

    #[test]
    fn transfer_reports_unsupported_and_empty_batches() {
        let bare = I2cAdapter::new("bare", None);
        let mut buf = [0u8; 1];
        assert_eq!(
            bare.transfer(&mut [I2cMsg::new(0x50, 0, &mut buf)]),
            -EOPNOTSUPP
        );
        let algo = scripted([0; 4]);
        let adap = I2cAdapter::new("ok", Some(&algo));
        assert_eq!(adap.transfer(&mut []), -EINVAL);
    }

    #[test]
    fn retries_only_on_eagain_and_stop_at_the_retry_count() {
        let algo = scripted([-EAGAIN, -EAGAIN, 1, 1]);
        let mut adap = I2cAdapter::new("retry", Some(&algo));
        adap.retries = 3;
        adap.timeout = 1_000;
        let mut buf = [0u8; 1];
        assert_eq!(adap.transfer(&mut [I2cMsg::new(0x50, 0, &mut buf)]), 1);
        assert_eq!(algo.calls.load(Ordering::Relaxed), 3);

        let algo = scripted([-EAGAIN; 4]);
        let mut adap = I2cAdapter::new("exhaust", Some(&algo));
        adap.retries = 2;
        adap.timeout = 1_000;
        assert_eq!(
            adap.transfer(&mut [I2cMsg::new(0x50, 0, &mut buf)]),
            -EAGAIN
        );
        assert_eq!(algo.calls.load(Ordering::Relaxed), 3);

        let algo = scripted([-5, 1, 1, 1]);
        let mut adap = I2cAdapter::new("hard", Some(&algo));
        adap.retries = 3;
        adap.timeout = 1_000;
        assert_eq!(adap.transfer(&mut [I2cMsg::new(0x50, 0, &mut buf)]), -5);
        assert_eq!(algo.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn quirks_reject_oversized_and_malformed_batches_before_the_algorithm() {
        let algo = scripted([1, 1, 1, 1]);
        let quirks = I2cAdapterQuirks {
            flags: I2C_AQ_NO_ZERO_LEN_READ,
            max_num_msgs: 2,
            max_read_len: 4,
            ..Default::default()
        };
        let mut adap = I2cAdapter::new("quirky", Some(&algo));
        adap.quirks = Some(&quirks);
        let mut small = [0u8; 4];
        let mut big = [0u8; 8];
        assert_eq!(
            adap.transfer(&mut [I2cMsg::new(0x50, I2C_M_RD, &mut big)]),
            -EOPNOTSUPP
        );
        assert_eq!(
            adap.transfer(&mut [I2cMsg::new(0x50, I2C_M_RD, &mut small[..0])]),
            -EOPNOTSUPP
        );
        let mut a = [0u8; 1];
        let mut b = [0u8; 1];
        let mut c = [0u8; 1];
        assert_eq!(
            adap.transfer(&mut [
                I2cMsg::new(0x50, 0, &mut a),
                I2cMsg::new(0x50, 0, &mut b),
                I2cMsg::new(0x50, 0, &mut c),
            ]),
            -EOPNOTSUPP
        );
        assert_eq!(algo.calls.load(Ordering::Relaxed), 0);
        assert_eq!(adap.transfer(&mut [I2cMsg::new(0x50, 0, &mut a)]), 1);
    }
}
