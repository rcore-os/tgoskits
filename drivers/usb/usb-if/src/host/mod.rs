use crate::transfer::{Recipient, Request, RequestType};

pub mod hub;

#[derive(Debug, Clone)]
pub struct ControlSetup {
    pub request_type: RequestType,
    pub recipient: Recipient,
    pub request: Request,
    pub value: u16,
    pub index: u16,
}
