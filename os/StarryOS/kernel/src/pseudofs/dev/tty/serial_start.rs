use core::sync::atomic::{AtomicU8, Ordering};

const SERIAL_DEFAULT_BAUDRATE: u32 = 115_200;
const START_MODE_UNASSIGNED: u8 = 0;

/// Selects whether runtime startup adopts an existing boot UART configuration
/// or initializes an ordinary serial port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum SerialStartMode {
    /// Preserve the firmware/someboot line configuration for `/dev/console`.
    AdoptBootConfiguration = 1,
    /// Configure a serial port that is not the active boot console.
    ConfigurePort = 2,
}

/// Hardware state to restore when OS IRQ activation fails after UART startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailedStartRecovery {
    /// Mask runtime IRQs while retaining the boot console's polling setup.
    RestoreBootPolling,
    /// Fully stop a runtime-configured non-console port.
    ShutdownPort,
}

/// Error returned when assigning or querying an immutable startup policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SerialStartPolicyError {
    /// The serial registry has not assigned a role yet.
    Unassigned,
    /// A caller attempted to replace an already-published role.
    AlreadyAssigned,
}

/// One-time startup policy assigned after console selection.
///
/// Keeping this policy in the backend prevents an ordinary open from
/// configuring the boot UART before the console handover is committed.
pub(crate) struct SerialStartPolicy {
    mode: AtomicU8,
}

impl SerialStartPolicy {
    pub(crate) const fn new() -> Self {
        Self {
            mode: AtomicU8::new(START_MODE_UNASSIGNED),
        }
    }

    pub(crate) fn assign(&self, mode: SerialStartMode) -> Result<(), SerialStartPolicyError> {
        self.mode
            .compare_exchange(
                START_MODE_UNASSIGNED,
                mode as u8,
                Ordering::Release,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| SerialStartPolicyError::AlreadyAssigned)
    }

    pub(crate) fn mode(&self) -> Result<SerialStartMode, SerialStartPolicyError> {
        match self.mode.load(Ordering::Acquire) {
            value if value == SerialStartMode::AdoptBootConfiguration as u8 => {
                Ok(SerialStartMode::AdoptBootConfiguration)
            }
            value if value == SerialStartMode::ConfigurePort as u8 => {
                Ok(SerialStartMode::ConfigurePort)
            }
            _ => Err(SerialStartPolicyError::Unassigned),
        }
    }
}

impl SerialStartMode {
    /// Resolves the optional baudrate passed to the raw serial driver.
    ///
    /// Boot-console adoption deliberately does not invoke `read_current`. Apart
    /// from avoiding needless register access, this prevents a runtime clock
    /// model from turning an already-correct boot divisor into a different line
    /// rate during ownership handover.
    pub(crate) fn startup_baudrate(self, read_current: impl FnOnce() -> u32) -> Option<u32> {
        match self {
            Self::AdoptBootConfiguration => None,
            Self::ConfigurePort => {
                let current = read_current();
                Some(if current == 0 {
                    SERIAL_DEFAULT_BAUDRATE
                } else {
                    current
                })
            }
        }
    }

    pub(crate) const fn failed_start_recovery(self) -> FailedStartRecovery {
        match self {
            Self::AdoptBootConfiguration => FailedStartRecovery::RestoreBootPolling,
            Self::ConfigurePort => FailedStartRecovery::ShutdownPort,
        }
    }
}
