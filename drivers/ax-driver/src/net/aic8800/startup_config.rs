//! Build-time station policy for the AKA AIC8800 attachment.

use pbkdf2::pbkdf2_hmac_array;
use rd_net::{WifiTransaction, Wpa2Pmk};
use sha1::Sha1;

#[path = "../../../build_support/wifi.rs"]
mod values;

pub(super) use values::StartupWifiConfigError;

pub(super) fn transaction() -> Result<Option<WifiTransaction>, StartupWifiConfigError> {
    from_values(
        option_env!("STARRY_WIFI_SSID"),
        option_env!("STARRY_WIFI_PASSWORD"),
    )
}

fn from_values(
    ssid: Option<&str>,
    password: Option<&str>,
) -> Result<Option<WifiTransaction>, StartupWifiConfigError> {
    let Some((ssid, password)) = values::validate(ssid, password)? else {
        return Ok(None);
    };
    let pmk = pbkdf2_hmac_array::<Sha1, 32>(password.as_bytes(), ssid.as_bytes(), 4096);
    Ok(Some(WifiTransaction::connect_wpa2_pmk(
        ssid,
        Wpa2Pmk::new(pmk),
    )))
}

#[cfg(test)]
mod tests {
    use rd_net::{WifiOperation, Wpa2Pmk};

    use super::{StartupWifiConfigError, from_values};

    #[test]
    fn compile_time_station_requires_a_complete_valid_pair() {
        assert_eq!(
            from_values(Some("ssid"), None).unwrap_err(),
            StartupWifiConfigError::Incomplete
        );
        assert_eq!(
            from_values(None, Some("password")).unwrap_err(),
            StartupWifiConfigError::Incomplete
        );
        assert_eq!(
            from_values(Some(""), Some("password")).unwrap_err(),
            StartupWifiConfigError::InvalidSsid
        );
        assert_eq!(
            from_values(Some("ssid"), Some("short")).unwrap_err(),
            StartupWifiConfigError::InvalidPassword
        );
    }

    #[test]
    fn compile_time_station_derives_the_ieee_wpa2_pmk_vector() {
        let transaction = from_values(Some("IEEE"), Some("password"))
            .unwrap()
            .unwrap();
        let WifiOperation::Connect { ssid, pmk, entropy } = transaction.operation() else {
            panic!("expected station transaction");
        };
        assert_eq!(ssid, "IEEE");
        assert_eq!(entropy, &None);
        assert_eq!(
            pmk,
            &Some(Wpa2Pmk::new([
                0xf4, 0x2c, 0x6f, 0xc5, 0x2d, 0xf0, 0xeb, 0xef, 0x9e, 0xbb, 0x4b, 0x90, 0xb3, 0x8a,
                0x5f, 0x90, 0x2e, 0x83, 0xfe, 0x1b, 0x13, 0x5a, 0x70, 0xe2, 0x3a, 0xed, 0x76, 0x2e,
                0x97, 0x10, 0xa1, 0x2e,
            ]))
        );
        assert!(transaction.needs_connect_entropy());
    }
}
