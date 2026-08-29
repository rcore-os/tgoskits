//! Validated chip-specific transport and register profiles.

use crate::{common::ChipVariant, registers::RegisterMap};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportHeader {
    Zero,
    Crc8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportGeneration {
    V1,
    V3,
}

impl TransportHeader {
    pub(crate) const fn uses_crc(self) -> bool {
        matches!(self, Self::Crc8)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FirmwareProfile {
    Aic8800Dc,
    Aic8800D80,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MailboxFlowPolicy {
    Direct,
    CreditGated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DataTxFlowPolicy {
    Direct,
    CreditGated,
}

pub(crate) struct ChipProfile {
    variant: ChipVariant,
    registers: RegisterMap,
    command_function: u8,
    data_function: u8,
    transport: TransportGeneration,
    transport_header: TransportHeader,
    mailbox_flow: MailboxFlowPolicy,
    data_tx_flow: DataTxFlowPolicy,
    firmware: FirmwareProfile,
    functions: &'static [u8],
}

impl ChipProfile {
    pub(crate) const fn for_variant(variant: ChipVariant) -> Option<&'static Self> {
        match variant {
            ChipVariant::Aic8800DC => Some(&AIC8800DC_PROFILE),
            ChipVariant::Aic8800D80 => Some(&AIC8800D80_PROFILE),
            ChipVariant::Aic8801
            | ChipVariant::Aic8800DW
            | ChipVariant::Aic8800D80X2
            | ChipVariant::Unknown => None,
        }
    }

    pub(crate) const fn variant(&self) -> ChipVariant {
        self.variant
    }

    pub(crate) const fn registers(&self) -> RegisterMap {
        self.registers
    }

    pub(crate) const fn command_function(&self) -> u8 {
        self.command_function
    }

    pub(crate) const fn data_function(&self) -> u8 {
        self.data_function
    }

    pub(crate) const fn transport_header(&self) -> TransportHeader {
        self.transport_header
    }

    pub(crate) const fn transport(&self) -> TransportGeneration {
        self.transport
    }

    pub(crate) const fn firmware(&self) -> FirmwareProfile {
        self.firmware
    }

    pub(crate) const fn mailbox_flow(&self) -> MailboxFlowPolicy {
        self.mailbox_flow
    }

    pub(crate) const fn data_tx_flow(&self) -> DataTxFlowPolicy {
        self.data_tx_flow
    }

    pub(crate) const fn function(&self, index: usize) -> Option<u8> {
        if index < self.functions.len() {
            Some(self.functions[index])
        } else {
            None
        }
    }
}

const DC_FUNCTIONS: &[u8] = &[1, 2];
const D80_FUNCTIONS: &[u8] = &[1];

static AIC8800DC_PROFILE: ChipProfile = ChipProfile {
    variant: ChipVariant::Aic8800DC,
    registers: RegisterMap::v1(),
    command_function: 2,
    data_function: 1,
    transport: TransportGeneration::V1,
    transport_header: TransportHeader::Zero,
    mailbox_flow: MailboxFlowPolicy::Direct,
    data_tx_flow: DataTxFlowPolicy::Direct,
    firmware: FirmwareProfile::Aic8800Dc,
    functions: DC_FUNCTIONS,
};

static AIC8800D80_PROFILE: ChipProfile = ChipProfile {
    variant: ChipVariant::Aic8800D80,
    registers: RegisterMap::v3(),
    command_function: 1,
    data_function: 1,
    transport: TransportGeneration::V3,
    transport_header: TransportHeader::Crc8,
    mailbox_flow: MailboxFlowPolicy::CreditGated,
    data_tx_flow: DataTxFlowPolicy::CreditGated,
    firmware: FirmwareProfile::Aic8800D80,
    functions: D80_FUNCTIONS,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_variants_have_no_implicit_v1_profile() {
        for variant in [
            ChipVariant::Aic8801,
            ChipVariant::Aic8800DW,
            ChipVariant::Aic8800D80X2,
            ChipVariant::Unknown,
        ] {
            assert!(ChipProfile::for_variant(variant).is_none());
        }
    }
}
