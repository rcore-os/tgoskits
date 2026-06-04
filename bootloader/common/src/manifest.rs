use core::str;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootManifest<'a> {
    pub kernel_url: &'a str,
    pub kernel_size: u64,
    pub kernel_load_addr: u64,
    pub entry_point: u64,
    pub arch: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    MissingField(&'static str),
    InvalidJson(&'static str),
    InvalidNumber(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadError {
    EmptyBody,
    BodyTooLarge,
    NonUtf8Body,
    InvalidManifest(ManifestError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    EmptyUrl,
    MissingPathSeparator,
    OutputTooSmall,
    MalformedDevicePath,
    NonUtf8Uri,
    UriNotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseNumberError {
    InvalidDigit,
    Empty,
    Overflow,
}

pub fn parse_manifest(input: &str) -> Result<BootManifest<'_>, ManifestError> {
    Ok(BootManifest {
        kernel_url: json_string_field(input, "kernel_url")?,
        kernel_size: json_u64_field(input, "kernel_size")?,
        kernel_load_addr: parse_addr(json_string_field(input, "kernel_load_addr")?)
            .map_err(|_| ManifestError::InvalidNumber("kernel_load_addr"))?,
        entry_point: parse_addr(json_string_field(input, "entry_point")?)
            .map_err(|_| ManifestError::InvalidNumber("entry_point"))?,
        arch: json_string_field(input, "arch")?,
    })
}

pub fn parse_downloaded_manifest(
    body: &[u8],
    max_len: usize,
) -> Result<BootManifest<'_>, DownloadError> {
    if body.is_empty() {
        return Err(DownloadError::EmptyBody);
    }
    if body.len() > max_len {
        return Err(DownloadError::BodyTooLarge);
    }

    let manifest = str::from_utf8(body).map_err(|_| DownloadError::NonUtf8Body)?;
    parse_manifest(manifest).map_err(DownloadError::InvalidManifest)
}

pub fn write_sibling_manifest_url<'a>(
    loader_url: &str,
    output: &'a mut [u8],
) -> Result<&'a str, UrlError> {
    let loader_url = loader_url.trim();
    if loader_url.is_empty() {
        return Err(UrlError::EmptyUrl);
    }

    let slash = loader_url
        .rfind('/')
        .ok_or(UrlError::MissingPathSeparator)?;
    let prefix = &loader_url[..slash + 1];
    let needed = prefix.len() + b"manifest.json".len();
    if needed > output.len() {
        return Err(UrlError::OutputTooSmall);
    }

    output[..prefix.len()].copy_from_slice(prefix.as_bytes());
    output[prefix.len()..needed].copy_from_slice(b"manifest.json");

    str::from_utf8(&output[..needed]).map_err(|_| UrlError::NonUtf8Uri)
}

pub fn uri_from_device_path(device_path: &[u8]) -> Result<&str, UrlError> {
    const DEVICE_PATH_TYPE_MESSAGING: u8 = 0x03;
    const DEVICE_PATH_TYPE_END: u8 = 0x7f;
    const DEVICE_PATH_SUBTYPE_URI: u8 = 0x18;
    const DEVICE_PATH_SUBTYPE_END_ENTIRE: u8 = 0xff;

    let mut offset = 0;
    let mut uri = None;

    while offset + 4 <= device_path.len() {
        let node_type = device_path[offset];
        let node_subtype = device_path[offset + 1];
        let node_len =
            u16::from_le_bytes([device_path[offset + 2], device_path[offset + 3]]) as usize;
        if node_len < 4 || offset + node_len > device_path.len() {
            return Err(UrlError::MalformedDevicePath);
        }

        if node_type == DEVICE_PATH_TYPE_MESSAGING && node_subtype == DEVICE_PATH_SUBTYPE_URI {
            let payload = trim_trailing_nul(&device_path[offset + 4..offset + node_len]);
            uri = Some(str::from_utf8(payload).map_err(|_| UrlError::NonUtf8Uri)?);
        }

        offset += node_len;
        if node_type == DEVICE_PATH_TYPE_END && node_subtype == DEVICE_PATH_SUBTYPE_END_ENTIRE {
            return uri.ok_or(UrlError::UriNotFound);
        }
    }

    Err(UrlError::MalformedDevicePath)
}

pub fn parse_addr(input: &str) -> Result<u64, ParseNumberError> {
    let value = input.trim();
    let (radix, digits) = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        (16, hex)
    } else {
        (10, value)
    };

    parse_u64_digits(digits, radix)
}

fn trim_trailing_nul(bytes: &[u8]) -> &[u8] {
    match bytes.iter().rposition(|byte| *byte != 0) {
        Some(last) => &bytes[..=last],
        None => &[],
    }
}

fn json_string_field<'a>(input: &'a str, key: &'static str) -> Result<&'a str, ManifestError> {
    let value = field_value(input, key)?;
    parse_json_string(value).ok_or(ManifestError::InvalidJson(key))
}

fn json_u64_field(input: &str, key: &'static str) -> Result<u64, ManifestError> {
    let value = field_value(input, key)?;
    let end = value
        .bytes()
        .position(|byte| !byte.is_ascii_digit() && byte != b'_')
        .unwrap_or(value.len());
    if end == 0 {
        return Err(ManifestError::InvalidNumber(key));
    }
    parse_u64_digits(&value[..end], 10).map_err(|_| ManifestError::InvalidNumber(key))
}

fn field_value<'a>(input: &'a str, key: &'static str) -> Result<&'a str, ManifestError> {
    let key_start = find_json_key(input, key).ok_or(ManifestError::MissingField(key))?;
    let after_key = &input[key_start + key.len() + 2..];
    let colon = after_key
        .bytes()
        .position(|byte| byte == b':')
        .ok_or(ManifestError::InvalidJson(key))?;
    Ok(after_key[colon + 1..].trim_start())
}

fn find_json_key(input: &str, key: &str) -> Option<usize> {
    let quoted_len = key.len() + 2;
    let bytes = input.as_bytes();
    let mut index = 0;

    while index + quoted_len <= bytes.len() {
        if bytes[index] == b'"'
            && input[index + 1..].starts_with(key)
            && bytes.get(index + quoted_len - 1) == Some(&b'"')
        {
            return Some(index);
        }
        index += 1;
    }

    None
}

fn parse_json_string(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }

    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => return None,
            b'"' => return Some(&input[1..index]),
            _ => index += 1,
        }
    }

    None
}

fn parse_u64_digits(input: &str, radix: u32) -> Result<u64, ParseNumberError> {
    let mut value = 0u64;
    let mut saw_digit = false;

    for byte in input.bytes() {
        if byte == b'_' {
            continue;
        }
        let digit = match byte {
            b'0'..=b'9' => (byte - b'0') as u32,
            b'a'..=b'f' => (byte - b'a' + 10) as u32,
            b'A'..=b'F' => (byte - b'A' + 10) as u32,
            _ => return Err(ParseNumberError::InvalidDigit),
        };
        if digit >= radix {
            return Err(ParseNumberError::InvalidDigit);
        }
        value = value
            .checked_mul(radix as u64)
            .and_then(|value| value.checked_add(digit as u64))
            .ok_or(ParseNumberError::Overflow)?;
        saw_digit = true;
    }

    saw_digit.then_some(value).ok_or(ParseNumberError::Empty)
}

#[cfg(test)]
mod tests {
    use super::{
        BootManifest, DownloadError, ManifestError, UrlError, parse_addr,
        parse_downloaded_manifest, parse_manifest, uri_from_device_path,
        write_sibling_manifest_url,
    };

    #[test]
    fn parses_server_manifest() {
        let manifest = parse_manifest(
            r#"{
                "kernel_url": "http://127.0.0.1:2999/boot/boards/demo/current/kernel.bin",
                "kernel_size": 123456,
                "kernel_load_addr": "0x20_3008_0000",
                "entry_point": "0x20_3008_0000",
                "arch": "loongarch64"
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest,
            BootManifest {
                kernel_url: "http://127.0.0.1:2999/boot/boards/demo/current/kernel.bin",
                kernel_size: 123456,
                kernel_load_addr: 0x20_3008_0000,
                entry_point: 0x20_3008_0000,
                arch: "loongarch64",
            }
        );
    }

    #[test]
    fn parses_decimal_and_hex_addresses() {
        assert_eq!(parse_addr("2097152"), Ok(0x20_0000));
        assert_eq!(parse_addr("0x20_0000"), Ok(0x20_0000));
    }

    #[test]
    fn rejects_missing_fields() {
        let err = parse_manifest(r#"{"kernel_size": 1}"#).unwrap_err();
        assert_eq!(err, ManifestError::MissingField("kernel_url"));
    }

    #[test]
    fn rejects_escaped_manifest_strings_for_now() {
        let err = parse_manifest(
            r#"{
                "kernel_url": "http:\/\/127.0.0.1\/kernel.bin",
                "kernel_size": 1,
                "kernel_load_addr": "0x200000",
                "entry_point": "0x200000",
                "arch": "x86_64"
            }"#,
        )
        .unwrap_err();

        assert_eq!(err, ManifestError::InvalidJson("kernel_url"));
    }

    #[test]
    fn parses_downloaded_manifest_bytes() {
        let manifest = parse_downloaded_manifest(
            br#"{
                "kernel_url": "http://127.0.0.1:2999/kernel.bin",
                "kernel_size": 4096,
                "kernel_load_addr": "0x200000",
                "entry_point": "0x200000",
                "arch": "x86_64"
            }"#,
            1024,
        )
        .unwrap();

        assert_eq!(manifest.kernel_url, "http://127.0.0.1:2999/kernel.bin");
        assert_eq!(manifest.kernel_size, 4096);
        assert_eq!(manifest.kernel_load_addr, 0x20_0000);
        assert_eq!(manifest.entry_point, 0x20_0000);
        assert_eq!(manifest.arch, "x86_64");
    }

    #[test]
    fn rejects_empty_or_oversized_downloaded_manifest() {
        assert_eq!(
            parse_downloaded_manifest(b"", 1024),
            Err(DownloadError::EmptyBody)
        );
        assert_eq!(
            parse_downloaded_manifest(br#"{"kernel_size":1}"#, 4),
            Err(DownloadError::BodyTooLarge)
        );
    }

    #[test]
    fn rejects_non_utf8_downloaded_manifest() {
        assert_eq!(
            parse_downloaded_manifest(&[0xff, 0xfe], 1024),
            Err(DownloadError::NonUtf8Body)
        );
    }

    #[test]
    fn wraps_downloaded_manifest_parse_errors() {
        assert_eq!(
            parse_downloaded_manifest(br#"{"kernel_size":1}"#, 1024),
            Err(DownloadError::InvalidManifest(ManifestError::MissingField(
                "kernel_url"
            )))
        );
    }

    #[test]
    fn builds_manifest_url_next_to_loader_url() {
        let mut output = [0u8; 128];
        let manifest_url = write_sibling_manifest_url(
            "http://127.0.0.1:2999/boot/boards/demo/current/BOOTX64.EFI",
            &mut output,
        )
        .unwrap();

        assert_eq!(
            manifest_url,
            "http://127.0.0.1:2999/boot/boards/demo/current/manifest.json"
        );
    }

    #[test]
    fn rejects_manifest_url_output_that_is_too_small() {
        let mut output = [0u8; 8];
        let err = write_sibling_manifest_url("http://host/BOOTX64.EFI", &mut output).unwrap_err();
        assert_eq!(err, UrlError::OutputTooSmall);
    }

    #[test]
    fn extracts_uri_from_device_path() {
        let uri = b"http://host/current/BOOTX64.EFI";
        let uri_node_len = (4 + uri.len()) as u16;
        let mut path = std::vec::Vec::new();
        path.extend_from_slice(&[0x03, 0x18]);
        path.extend_from_slice(&uri_node_len.to_le_bytes());
        path.extend_from_slice(uri);
        path.extend_from_slice(&[0x7f, 0xff, 0x04, 0x00]);

        assert_eq!(
            uri_from_device_path(&path),
            Ok("http://host/current/BOOTX64.EFI")
        );
    }

    #[test]
    fn rejects_device_path_without_uri() {
        let path = [0x7f, 0xff, 0x04, 0x00];
        assert_eq!(uri_from_device_path(&path), Err(UrlError::UriNotFound));
    }
}
