use sbi_spec::rfnc::{REMOTE_FENCE_I, REMOTE_SFENCE_VMA};

use axerrno::AxResult;

#[derive(Clone, Copy, Debug)]
pub enum RemoteFenceFunction {
    FenceI {
        hart_mask: u64,
        hart_mask_base: u64,
    },
    RemoteSFenceVMA {
        hart_mask: u64,
        hart_mask_base: u64,
        start_addr: u64,
        size: u64,
    },
}

impl RemoteFenceFunction {
    pub fn from_args(args: &[usize]) -> AxResult<Self> {
        match args[6] {
            REMOTE_FENCE_I => Ok(Self::FenceI {
                hart_mask: args[0] as u64,
                hart_mask_base: args[1] as u64,
            }),
            REMOTE_SFENCE_VMA => Ok(Self::RemoteSFenceVMA {
                hart_mask: args[0] as u64,
                hart_mask_base: args[1] as u64,
                start_addr: args[2] as u64,
                size: args[3] as u64,
            }),
            _ => panic!("Unsupported yet!"),
        }
    }
}
