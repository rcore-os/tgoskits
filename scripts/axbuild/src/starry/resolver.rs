//! Resolver helpers for Starry guest prebuild staging roots.

use std::{fs, net::IpAddr, path::Path};

use anyhow::Context;

const HOST_RESOLV_CONF_PATH: &str = "/etc/resolv.conf";
const HOST_RESOLVED_CONF_PATH: &str = "/run/systemd/resolve/resolv.conf";
const DEFAULT_DNS_SERVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];

pub(crate) fn write_host_resolver_config(staging_root: &Path) -> anyhow::Result<()> {
    let resolv_conf = preferred_host_resolver_config()?;
    let output_path = staging_root.join("etc/resolv.conf");
    fs::write(&output_path, resolv_conf)
        .with_context(|| format!("failed to write {}", output_path.display()))
}

pub(crate) fn preferred_host_resolver_config() -> anyhow::Result<String> {
    if let Some(content) = read_usable_resolver_file(Path::new(HOST_RESOLVED_CONF_PATH))? {
        return Ok(content);
    }
    if let Some(content) = read_usable_resolver_file(Path::new(HOST_RESOLV_CONF_PATH))? {
        return Ok(content);
    }

    Ok(DEFAULT_DNS_SERVERS
        .iter()
        .map(|server| format!("nameserver {server}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn read_usable_resolver_file(path: &Path) -> anyhow::Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let usable = content
        .lines()
        .filter_map(parse_nameserver_line)
        .filter(|addr| !addr.is_loopback() && *addr != IpAddr::from([10, 0, 2, 3]))
        .map(|addr| format!("nameserver {addr}"))
        .collect::<Vec<_>>();

    if usable.is_empty() {
        Ok(None)
    } else {
        Ok(Some(usable.join("\n") + "\n"))
    }
}

pub(crate) fn parse_nameserver_line(line: &str) -> Option<IpAddr> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("nameserver"), Some(value), None) => value.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    #[test]
    fn preferred_resolver_filters_loopback_and_slirp_addresses() {
        let content = "nameserver 127.0.0.53\nnameserver 10.0.2.3\nnameserver 8.8.8.8\n";
        let usable = content
            .lines()
            .filter_map(parse_nameserver_line)
            .filter(|addr| !addr.is_loopback() && *addr != IpAddr::from([10, 0, 2, 3]))
            .map(|addr| format!("nameserver {addr}"))
            .collect::<Vec<_>>();
        assert_eq!(usable, vec!["nameserver 8.8.8.8".to_string()]);
    }
}
