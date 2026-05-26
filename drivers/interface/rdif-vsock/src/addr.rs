#[derive(Copy, Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct VsockAddr {
    pub cid: u64,
    pub port: u32,
}

#[derive(Copy, Clone, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct VsockConnId {
    pub peer_addr: VsockAddr,
    pub local_port: u32,
}

impl VsockConnId {
    pub const fn listening(local_port: u32) -> Self {
        Self {
            peer_addr: VsockAddr { cid: 0, port: 0 },
            local_port,
        }
    }
}
