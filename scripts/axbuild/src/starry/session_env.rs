use std::{collections::BTreeMap, env, fs, path::Path};

use anyhow::{Context, ensure};
use pbkdf2::pbkdf2_hmac_array;
use serde::Deserialize;
use sha1::Sha1;
use zeroize::{Zeroize, Zeroizing};

const CONFIG_NAME: &str = ".session-env.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionEnvConfig {
    #[serde(default)]
    pub inject_boot_entropy: bool,
    #[serde(default)]
    files: BTreeMap<String, String>,
    wifi: Option<WifiSessionConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WifiSessionConfig {
    ssid_file: String,
    pmk_file: String,
}

impl SessionEnvConfig {
    pub(super) fn load(case_dir: &Path, board_config_path: &Path) -> anyhow::Result<Option<Self>> {
        let board_sidecar = board_config_path.with_extension("session-env.toml");
        let path = if board_sidecar.is_file() {
            board_sidecar
        } else {
            case_dir.join(CONFIG_NAME)
        };
        if !path.is_file() {
            return Ok(None);
        }
        let config: Self = toml::from_str(
            &fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        )
        .with_context(|| format!("failed to parse {}", path.display()))?;
        for (relative_path, variable) in &config.files {
            super::test::validate_relative_path(Path::new(relative_path))?;
            ensure!(
                variable
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_' || byte.is_ascii_digit()),
                "session environment mapping contains an invalid variable name"
            );
            ensure!(
                !matches!(
                    variable.as_str(),
                    "STARRY_WIFI_SSID" | "STARRY_WIFI_PASSWORD"
                ),
                "Wi-Fi credentials must use the typed `[wifi]` session mapping"
            );
        }
        if let Some(wifi) = &config.wifi {
            super::test::validate_relative_path(Path::new(&wifi.ssid_file))?;
            super::test::validate_relative_path(Path::new(&wifi.pmk_file))?;
            ensure!(
                wifi.ssid_file != wifi.pmk_file,
                "Wi-Fi SSID and PMK session paths must differ"
            );
        }
        Ok(Some(config))
    }

    pub(super) fn validate_environment(&self) -> anyhow::Result<()> {
        for variable in self.files.values() {
            let value = env::var(variable).map_err(|_| {
                anyhow::anyhow!("required session environment variable `{variable}` is missing")
            })?;
            validate_value(variable, value.as_bytes())?;
        }
        if self.wifi.is_some() {
            let ssid = env::var("STARRY_WIFI_SSID").map_err(|_| {
                anyhow::anyhow!(
                    "required session environment variable `STARRY_WIFI_SSID` is missing"
                )
            })?;
            let password = Zeroizing::new(env::var("STARRY_WIFI_PASSWORD").map_err(|_| {
                anyhow::anyhow!(
                    "required session environment variable `STARRY_WIFI_PASSWORD` is missing"
                )
            })?);
            validate_value("STARRY_WIFI_SSID", ssid.as_bytes())?;
            validate_value("STARRY_WIFI_PASSWORD", password.as_bytes())?;
        }
        Ok(())
    }

    pub(super) fn materialize(&self, upload_root: &Path) -> anyhow::Result<()> {
        for (relative_path, variable) in &self.files {
            let value = env::var(variable).map_err(|_| {
                anyhow::anyhow!("required session environment variable `{variable}` is missing")
            })?;
            validate_value(variable, value.as_bytes())?;
            write_session_asset(upload_root, relative_path, value.as_bytes())?;
        }
        if let Some(wifi) = &self.wifi {
            let ssid = env::var("STARRY_WIFI_SSID").map_err(|_| {
                anyhow::anyhow!(
                    "required session environment variable `STARRY_WIFI_SSID` is missing"
                )
            })?;
            let password = Zeroizing::new(env::var("STARRY_WIFI_PASSWORD").map_err(|_| {
                anyhow::anyhow!(
                    "required session environment variable `STARRY_WIFI_PASSWORD` is missing"
                )
            })?);
            validate_value("STARRY_WIFI_SSID", ssid.as_bytes())?;
            validate_value("STARRY_WIFI_PASSWORD", password.as_bytes())?;
            let mut pmk = derive_wifi_pmk(ssid.as_bytes(), password.as_bytes());
            write_session_asset(upload_root, &wifi.ssid_file, ssid.as_bytes())?;
            let result = write_session_asset(upload_root, &wifi.pmk_file, &pmk);
            pmk.zeroize();
            result?;
        }
        Ok(())
    }
}

fn write_session_asset(
    upload_root: &Path,
    relative_path: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let destination = upload_root.join(relative_path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    use std::io::Write;
    let mut file = options
        .open(&destination)
        .context("failed to open session environment asset")?;
    file.write_all(bytes)
        .context("failed to write session environment asset")?;
    file.sync_all()
        .context("failed to persist session environment asset")
}

fn derive_wifi_pmk(ssid: &[u8], password: &[u8]) -> [u8; 32] {
    pbkdf2_hmac_array::<Sha1, 32>(password, ssid, 4096)
}

fn validate_value(variable: &str, value: &[u8]) -> anyhow::Result<()> {
    ensure!(
        !value.contains(&0) && !value.contains(&b'\n') && !value.contains(&b'\r'),
        "session environment variable `{variable}` contains a forbidden byte"
    );
    match variable {
        "STARRY_WIFI_SSID" => ensure!(
            !value.is_empty() && value.len() <= 32,
            "STARRY_WIFI_SSID must contain 1..=32 bytes"
        ),
        "STARRY_WIFI_PASSWORD" => ensure!(
            (8..=63).contains(&value.len()) && value.iter().all(u8::is_ascii_graphic),
            "STARRY_WIFI_PASSWORD must contain 8..=63 printable ASCII bytes"
        ),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{derive_wifi_pmk, validate_value};

    #[test]
    fn wifi_credentials_are_validated_without_echoing_values() {
        assert!(validate_value("STARRY_WIFI_SSID", &[b'x'; 33]).is_err());
        let error = validate_value("STARRY_WIFI_PASSWORD", b"short").unwrap_err();
        assert!(!error.to_string().contains("short"));
        assert!(validate_value("STARRY_WIFI_PASSWORD", b"abcdefgh").is_ok());
    }

    #[test]
    fn host_side_pmk_derivation_matches_the_ieee_vector() {
        assert_eq!(
            derive_wifi_pmk(b"IEEE", b"password"),
            [
                0xf4, 0x2c, 0x6f, 0xc5, 0x2d, 0xf0, 0xeb, 0xef, 0x9e, 0xbb, 0x4b, 0x90, 0xb3, 0x8a,
                0x5f, 0x90, 0x2e, 0x83, 0xfe, 0x1b, 0x13, 0x5a, 0x70, 0xe2, 0x3a, 0xed, 0x76, 0x2e,
                0x97, 0x10, 0xa1, 0x2e,
            ]
        );
    }
}
