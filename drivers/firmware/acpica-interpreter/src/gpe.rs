//! Owned _PRW capability parsing; no suspend or wake-mask activation implied.
use alloc::{ffi::CString, string::String};
use core::ffi::c_char;

use crate::{BAD_PARAMETER, Engine, Status, Value};
#[derive(Debug, PartialEq, Eq)]
pub struct WakeGpe {
    pub block: Option<String>,
    pub number: u32,
    pub deepest_state: u8,
}
pub fn parse(value: Value) -> Result<WakeGpe, Status> {
    let Value::Package(fields) = value else {
        return Err(BAD_PARAMETER);
    };
    if fields.len() < 2 {
        return Err(BAD_PARAMETER);
    }
    let Value::Integer(state) = &fields[1] else {
        return Err(BAD_PARAMETER);
    };
    if *state > 5 {
        return Err(BAD_PARAMETER);
    }
    let (block, number) = match &fields[0] {
        Value::Integer(number) => (None, *number),
        Value::Package(gpe) if gpe.len() == 2 => {
            let (Value::Reference(path), Value::Integer(number)) = (&gpe[0], &gpe[1]) else {
                return Err(BAD_PARAMETER);
            };
            let path = core::str::from_utf8(path).map_err(|_| BAD_PARAMETER)?;
            if !path.starts_with('\\') || path.len() > 4096 || path.as_bytes().contains(&0) {
                return Err(BAD_PARAMETER);
            }
            (Some(String::from(path)), *number)
        }
        _ => return Err(BAD_PARAMETER),
    };
    Ok(WakeGpe {
        block,
        number: number.try_into().map_err(|_| BAD_PARAMETER)?,
        deepest_state: *state as u8,
    })
}
unsafe extern "C" {
    fn acpica_interpreter_setup_wake(
        device: *const c_char,
        block: *const c_char,
        number: u32,
        runtime: u8,
    ) -> Status;
}
impl Engine {
    /// Register _PRW capability. Runtime=true explicitly admits S0 delivery;
    /// neither path enables a sleep-state wake mask or executes power resources.
    pub fn setup_wake_gpe(&self, device: &str, gpe: &WakeGpe, runtime: bool) -> Result<(), Status> {
        let device = CString::new(device).map_err(|_| BAD_PARAMETER)?;
        let block = gpe
            .block
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| BAD_PARAMETER)?;
        // SAFETY: live engine; bridge resolves paths under ACPICA's namespace API.
        crate::engine::status(unsafe {
            acpica_interpreter_setup_wake(
                device.as_ptr(),
                block.as_ref().map_or(core::ptr::null(), |b| b.as_ptr()),
                gpe.number,
                u8::from(runtime),
            )
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_and_block_wake_sources() {
        assert_eq!(
            parse(Value::Package(alloc::vec![
                Value::Integer(3),
                Value::Integer(0)
            ])),
            Ok(WakeGpe {
                block: None,
                number: 3,
                deepest_state: 0
            })
        );
        let p = Value::Package(alloc::vec![
            Value::Package(alloc::vec![
                Value::Reference(b"\\GPE0".to_vec()),
                Value::Integer(7)
            ]),
            Value::Integer(3)
        ]);
        assert_eq!(parse(p).unwrap().block.as_deref(), Some("\\GPE0"));
    }
    #[test]
    fn malformed_wake_sources() {
        for v in [
            Value::Integer(0),
            Value::Package(alloc::vec![Value::Integer(u64::MAX), Value::Integer(3)]),
            Value::Package(alloc::vec![Value::Integer(1), Value::Integer(6)]),
        ] {
            assert!(parse(v).is_err());
        }
    }
}

/// Fail-closed SCI budget. The owner serializes access; only one recovery is
/// admitted over the entire boot, not one recovery for every new second.
pub struct StormBudget {
    window: u64,
    count: u32,
    recovered: bool,
}
impl StormBudget {
    pub const fn new() -> Self {
        Self {
            window: 0,
            count: 0,
            recovered: false,
        }
    }
    pub fn observe(&mut self, second: u64) -> bool {
        if self.window != second {
            self.window = second;
            self.count = 0
        }
        let storm = self.count >= 1024;
        self.count = self.count.saturating_add(1);
        storm
    }
    pub fn recover_once(&mut self) -> bool {
        if self.recovered {
            return false;
        }
        self.recovered = true;
        self.count = 0;
        true
    }
}
impl Default for StormBudget {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod storm_tests {
    use super::*;
    #[test]
    fn rate_and_lifetime_recovery_limits() {
        let mut b = StormBudget::new();
        for _ in 0..1024 {
            assert!(!b.observe(1))
        }
        assert!(b.observe(1));
        assert!(b.recover_once());
        for _ in 0..1024 {
            assert!(!b.observe(1))
        }
        assert!(b.observe(1));
        assert!(!b.recover_once());
        assert!(!b.observe(2));
        assert!(!b.recover_once());
    }
}
