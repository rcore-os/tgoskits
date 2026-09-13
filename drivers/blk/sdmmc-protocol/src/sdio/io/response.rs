use crate::{
    error::{Error, ErrorContext, Phase},
    response::{Response, SdioRwResponse},
};

pub(super) fn expect_r1(response: Response, command: u8) -> Result<(), Error> {
    match response {
        Response::R1(_) | Response::R1b(_) => Ok(()),
        _ => Err(bad_response(command)),
    }
}

pub(super) fn check_r5(response: SdioRwResponse, command: u8) -> Result<u8, Error> {
    let flags = response.flags();
    if flags & (1 << 7) != 0 {
        return Err(Error::Crc(ErrorContext::for_cmd(
            Phase::ResponseWait,
            command,
        )));
    }
    if flags & (1 << 6) != 0 {
        return Err(Error::UnsupportedCommand);
    }
    if flags & ((1 << 3) | (1 << 1) | 1) != 0 {
        return Err(bad_response(command));
    }
    Ok(response.data())
}

pub(super) fn bad_response(command: u8) -> Error {
    Error::BadResponse(ErrorContext::for_cmd(Phase::ResponseWait, command))
}
