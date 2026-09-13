use core::time::Duration;

use aic8800::AicRdifOptions;
use rdif_eth::{NetIrqSourceId, WifiLinkPolicy, WifiTransaction};

#[test]
fn public_options_preserve_bounded_startup_policy() {
    let transaction = WifiTransaction::open_access_point(
        b"portable-ap".to_vec(),
        6,
        WifiLinkPolicy {
            ip: [192, 168, 50, 1],
            prefix_len: 24,
            dhcp_server_client_ip: Some([192, 168, 50, 2]),
        },
    );
    let options = AicRdifOptions::new(NetIrqSourceId::new(3))
        .with_startup_delay(Duration::from_millis(1))
        .with_startup_timeout(Duration::from_secs(10))
        .with_control_timeout(Duration::from_secs(2))
        .with_startup_transaction(transaction.clone());

    assert_eq!(options.irq_source.get(), 3);
    assert_eq!(options.startup_delay, Duration::from_millis(1));
    assert_eq!(options.startup_timeout, Duration::from_secs(10));
    assert_eq!(options.control_timeout, Duration::from_secs(2));
    assert_eq!(options.startup_transaction, Some(transaction));
    assert!(options.queue_size >= 2);
}
