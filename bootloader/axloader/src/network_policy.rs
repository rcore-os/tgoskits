extern crate alloc;

use alloc::vec::Vec;

use httpboot_protocol::{LoaderDiscoveryOffer, PROTOCOL_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySelectionError {
    NoCompatibleServer,
    MultipleServers,
}

/// Selects a server only when every compatible reply identifies the same
/// server instance. This deliberately never makes a random choice.
pub fn select_unique_server(
    offers: Vec<LoaderDiscoveryOffer>,
) -> Result<LoaderDiscoveryOffer, DiscoverySelectionError> {
    let mut selected: Option<LoaderDiscoveryOffer> = None;
    for offer in offers {
        if offer.protocol_version != PROTOCOL_VERSION {
            continue;
        }
        if selected
            .as_ref()
            .is_some_and(|previous| previous.server_id != offer.server_id)
        {
            return Err(DiscoverySelectionError::MultipleServers);
        }
        selected.get_or_insert(offer);
    }
    selected.ok_or(DiscoverySelectionError::NoCompatibleServer)
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec};

    use httpboot_protocol::LEGACY_PROTOCOL_VERSION;

    use super::*;

    #[test]
    fn rejects_multiple_server_instances() {
        assert_eq!(
            select_unique_server(vec![
                offer("a", PROTOCOL_VERSION),
                offer("b", PROTOCOL_VERSION)
            ]),
            Err(DiscoverySelectionError::MultipleServers)
        );
    }

    #[test]
    fn ignores_other_protocol_versions() {
        assert_eq!(
            select_unique_server(vec![
                offer("old", LEGACY_PROTOCOL_VERSION),
                offer("current", PROTOCOL_VERSION)
            ])
            .unwrap()
            .server_id,
            "current"
        );
    }

    fn offer(server_id: &str, protocol_version: u16) -> LoaderDiscoveryOffer {
        LoaderDiscoveryOffer {
            protocol_version,
            server_id: server_id.to_string(),
            control_base_url: "http://10.77.0.1:2999".to_string(),
            registration_id: "registration".to_string(),
            expires_in_ms: 5_000,
        }
    }
}
