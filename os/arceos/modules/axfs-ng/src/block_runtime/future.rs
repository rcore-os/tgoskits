#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockIoFutureState {
    New,
    Submitted(RequestKey),
    Complete,
}

use super::RequestKey;
