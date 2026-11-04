//! Bounded VirtIO Sound wire encoders and parsers.

pub(crate) const CONTROL_QUEUE: u16 = 0;
pub(crate) const EVENT_QUEUE: u16 = 1;
pub(crate) const TX_QUEUE: u16 = 2;
pub(crate) const RX_QUEUE: u16 = 3;
pub(crate) const QUEUE_SIZE: usize = 32;

pub(crate) const STREAMS_CONFIG_OFFSET: usize = 4;
pub(crate) const MAX_STREAMS: u32 = 16;
pub(crate) const PCM_INFO_SIZE: usize = 32;
pub(crate) const PCM_STATUS_SIZE: usize = 8;
pub(crate) const STREAM_ID_SIZE: usize = 4;
pub(crate) const CONTROL_REQUEST_SIZE: usize = 64;
pub(crate) const CONTROL_RESPONSE_SIZE: usize = 4 + MAX_STREAMS as usize * PCM_INFO_SIZE;
pub(crate) const EVENT_SIZE: usize = 8;
pub(crate) const EVENT_BUFFER_COUNT: usize = 8;

pub(crate) const R_PCM_INFO: u32 = 0x0100;
pub(crate) const R_PCM_SET_PARAMS: u32 = 0x0101;
pub(crate) const R_PCM_PREPARE: u32 = 0x0102;
pub(crate) const R_PCM_RELEASE: u32 = 0x0103;
pub(crate) const R_PCM_START: u32 = 0x0104;
pub(crate) const R_PCM_STOP: u32 = 0x0105;

pub(crate) const S_OK: u32 = 0x8000;
pub(crate) const S_BAD_MSG: u32 = 0x8001;
pub(crate) const S_NOT_SUPP: u32 = 0x8002;
pub(crate) const S_IO_ERR: u32 = 0x8003;

pub(crate) const EVT_JACK_CONNECTED: u32 = 0x1000;
pub(crate) const EVT_JACK_DISCONNECTED: u32 = 0x1001;
pub(crate) const EVT_PCM_PERIOD_ELAPSED: u32 = 0x1100;
pub(crate) const EVT_PCM_XRUN: u32 = 0x1101;

pub(crate) const DIRECTION_OUTPUT: u8 = 0;
pub(crate) const DIRECTION_INPUT: u8 = 1;
pub(crate) const FORMAT_S16: u8 = 5;
pub(crate) const RATE_48000: u8 = 7;
pub(crate) const FORMAT_S16_BIT: u64 = 1 << 5;
pub(crate) const RATE_48000_BIT: u64 = 1 << 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseStatus {
    Ok,
    BadMessage,
    NotSupported,
    IoError,
    Unknown(u32),
}

impl ResponseStatus {
    pub(crate) fn from_raw(raw: u32) -> Self {
        match raw {
            S_OK => Self::Ok,
            S_BAD_MSG => Self::BadMessage,
            S_NOT_SUPP => Self::NotSupported,
            S_IO_ERR => Self::IoError,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PcmInfo {
    pub formats: u64,
    pub rates: u64,
    pub direction: u8,
    pub channels_min: u8,
    pub channels_max: u8,
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let raw: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let raw: [u8; 8] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u64::from_le_bytes(raw))
}

pub(crate) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Option<()> {
    let end = offset.checked_add(4)?;
    bytes
        .get_mut(offset..end)?
        .copy_from_slice(&value.to_le_bytes());
    Some(())
}

pub(crate) fn query_pcm_info_request(stream_count: u32) -> [u8; 16] {
    let mut request = [0_u8; 16];
    let _ = write_u32(&mut request, 0, R_PCM_INFO);
    let _ = write_u32(&mut request, 4, 0);
    let _ = write_u32(&mut request, 8, stream_count);
    let _ = write_u32(&mut request, 12, PCM_INFO_SIZE as u32);
    request
}

pub(crate) fn pcm_set_params_request(
    stream_id: u32,
    buffer_bytes: u32,
    period_bytes: u32,
    features: u32,
    channels: u8,
    format: u8,
    rate: u8,
) -> [u8; 24] {
    let mut request = [0_u8; 24];
    let _ = write_u32(&mut request, 0, R_PCM_SET_PARAMS);
    let _ = write_u32(&mut request, 4, stream_id);
    let _ = write_u32(&mut request, 8, buffer_bytes);
    let _ = write_u32(&mut request, 12, period_bytes);
    let _ = write_u32(&mut request, 16, features);
    request[20] = channels;
    request[21] = format;
    request[22] = rate;
    request
}

pub(crate) fn pcm_header_request(code: u32, stream_id: u32) -> [u8; 8] {
    let mut request = [0_u8; 8];
    let _ = write_u32(&mut request, 0, code);
    let _ = write_u32(&mut request, 4, stream_id);
    request
}

pub(crate) fn response_status(bytes: &[u8]) -> Option<ResponseStatus> {
    read_u32(bytes, 0).map(ResponseStatus::from_raw)
}

pub(crate) fn pcm_status(bytes: &[u8]) -> Option<(ResponseStatus, u32)> {
    Some((response_status(bytes)?, read_u32(bytes, 4)?))
}

pub(crate) fn parse_pcm_info(bytes: &[u8]) -> Option<PcmInfo> {
    if bytes.len() < PCM_INFO_SIZE {
        return None;
    }
    Some(PcmInfo {
        formats: read_u64(bytes, 8)?,
        rates: read_u64(bytes, 16)?,
        direction: *bytes.get(24)?,
        channels_min: *bytes.get(25)?,
        channels_max: *bytes.get(26)?,
    })
}

pub(crate) fn event(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() != EVENT_SIZE {
        return None;
    }
    Some((read_u32(bytes, 0)?, read_u32(bytes, 4)?))
}
