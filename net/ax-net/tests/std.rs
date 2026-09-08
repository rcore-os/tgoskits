extern crate alloc;

use alloc::boxed::Box;
use core::{cell::Cell, time::Duration};

use ax_io::IoError;
use ax_net::{
    InterfaceId, NetError, NetResult,
    options::{Configurable, GetSocketOption, SetSocketOption, TcpInfo, TcpInfoOptions, TcpState},
};
use ax_runtime as _;

#[test]
fn ax_net_interface_ids_validate_linux_ifindices() {
    let id = InterfaceId::new(7);
    assert_eq!(id.to_linux_ifindex(), 7);
    assert_eq!(InterfaceId::from_linux_ifindex(7), Some(id));
    assert_eq!(InterfaceId::from_linux_ifindex(0), None);
    assert_eq!(InterfaceId::from_linux_ifindex(-1), None);
}

struct MockConfigurable {
    supported: bool,
    set_calls: Cell<usize>,
}

impl Configurable for MockConfigurable {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> NetResult<bool> {
        match opt {
            GetSocketOption::ReuseAddress(value) => **value = true,
            GetSocketOption::SendTimeout(value) => **value = Duration::from_millis(7),
            GetSocketOption::TcpInfo(value) => {
                **value = TcpInfo {
                    state: TcpState::Established,
                    options: TcpInfoOptions::SACK | TcpInfoOptions::TIMESTAMPS,
                    snd_mss: 1460,
                    rcv_mss: 1460,
                    ..TcpInfo::default()
                };
            }
            _ => {}
        }
        Ok(self.supported)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> NetResult<bool> {
        if matches!(
            opt,
            SetSocketOption::NoDelay(true) | SetSocketOption::KeepAlive(true)
        ) {
            self.set_calls.set(self.set_calls.get() + 1);
        }
        Ok(self.supported)
    }
}

#[test]
fn ax_net_socket_options_dispatch_supported_and_unsupported_results() {
    let configurable = MockConfigurable {
        supported: true,
        set_calls: Cell::new(0),
    };

    let mut reuse = false;
    configurable
        .get_option(GetSocketOption::ReuseAddress(&mut reuse))
        .unwrap();
    assert!(reuse);

    let mut timeout = Duration::ZERO;
    configurable
        .get_option(GetSocketOption::SendTimeout(&mut timeout))
        .unwrap();
    assert_eq!(timeout, Duration::from_millis(7));

    let mut tcp_info = TcpInfo::default();
    configurable
        .get_option(GetSocketOption::TcpInfo(&mut tcp_info))
        .unwrap();
    assert_eq!(tcp_info.state, TcpState::Established);
    assert!(tcp_info.options.contains(TcpInfoOptions::SACK));
    assert_eq!(tcp_info.snd_mss, 1460);

    configurable
        .set_option(SetSocketOption::NoDelay(&true))
        .unwrap();
    configurable
        .set_option(SetSocketOption::KeepAlive(&true))
        .unwrap();
    assert_eq!(configurable.set_calls.get(), 2);

    let boxed: Box<dyn Configurable> = Box::new(MockConfigurable {
        supported: true,
        set_calls: Cell::new(0),
    });
    boxed.set_option(SetSocketOption::NoDelay(&true)).unwrap();

    let unsupported = MockConfigurable {
        supported: false,
        set_calls: Cell::new(0),
    };
    assert!(
        matches!(
            unsupported.set_option(SetSocketOption::NoDelay(&true)),
            Err(NetError::Unsupported)
        ) || unsupported
            .set_option(SetSocketOption::NoDelay(&true))
            .is_err()
    );
    let mut reuse = false;
    assert_eq!(
        unsupported
            .get_option(GetSocketOption::ReuseAddress(&mut reuse))
            .unwrap_err(),
        NetError::ProtocolOptionUnsupported
    );
}

#[test]
fn ax_net_interrupted_error_preserves_io_semantics() {
    assert_eq!(IoError::from(NetError::Interrupted), IoError::Interrupted);
}
