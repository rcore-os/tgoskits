use core::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupWifiConfigError {
    Incomplete,
    InvalidSsid,
    InvalidPassword,
}

impl fmt::Display for StartupWifiConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Incomplete => "both STARRY_WIFI_SSID and STARRY_WIFI_PASSWORD must be set",
            Self::InvalidSsid => {
                "STARRY_WIFI_SSID must contain 1..=32 bytes without NUL or line breaks"
            }
            Self::InvalidPassword => {
                "STARRY_WIFI_PASSWORD must contain 8..=63 printable ASCII bytes"
            }
        })
    }
}

pub fn validate<'a>(
    ssid: Option<&'a str>,
    password: Option<&'a str>,
) -> Result<Option<(&'a str, &'a str)>, StartupWifiConfigError> {
    let (ssid, password) = match (ssid, password) {
        (None, None) => return Ok(None),
        (Some(ssid), Some(password)) => (ssid, password),
        _ => return Err(StartupWifiConfigError::Incomplete),
    };

    let ssid_bytes = ssid.as_bytes();
    if ssid_bytes.is_empty()
        || ssid_bytes.len() > 32
        || ssid_bytes
            .iter()
            .any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
    {
        return Err(StartupWifiConfigError::InvalidSsid);
    }

    let password_bytes = password.as_bytes();
    if !(8..=63).contains(&password_bytes.len()) || !password_bytes.iter().all(u8::is_ascii_graphic)
    {
        return Err(StartupWifiConfigError::InvalidPassword);
    }

    Ok(Some((ssid, password)))
}
