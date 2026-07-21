//! Controller-owner placement regression tests.

use rdif_block::{ControlDomainActivation, IdList, OwnershipDomainId};

use super::*;

#[test]
fn shared_control_domain_stays_on_the_control_owner() {
    let domain = OwnershipDomainId::new(3).unwrap();
    let activation = ControlDomainActivation::SharedWithIo {
        domain,
        irq_sources: IdList::none(),
    };

    assert_eq!(
        domain_placement(activation, domain),
        DomainPlacement::ControlOwner
    );
}

#[test]
fn independent_control_never_claims_an_io_domain_thread() {
    let control = OwnershipDomainId::new(7).unwrap();
    let io = OwnershipDomainId::new(2).unwrap();
    let activation = ControlDomainActivation::Independent {
        domain: control,
        irq_sources: IdList::none(),
    };

    assert_eq!(
        domain_placement(activation, io),
        DomainPlacement::IndependentOwner
    );
}
