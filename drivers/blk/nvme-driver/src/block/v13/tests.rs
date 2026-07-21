use rdif_block::{DomainActivationPlan, LogicalDevicePublicationContract};

use super::*;

#[test]
fn discovery_capability_keeps_all_sixty_four_domain_queue_slots() {
    let identity = NonZeroUsize::new(0x1000).unwrap();
    let depth =
        HardwareQueueDepth::new(NonZeroU16::new(1).unwrap(), NonZeroU16::new(15).unwrap()).unwrap();
    let max_queues = NonZeroU16::new(64).unwrap();

    let (capabilities, topology) = build_interrupt_capabilities(
        identity,
        u64::MAX,
        depth,
        max_queues,
        NvmeInterruptMode::SharedIntx,
        &[0],
    )
    .unwrap();
    let domain = topology.domains[0].id;

    assert_eq!(capabilities.controller_identity(), identity);
    assert_eq!(capabilities.control_domain(), domain);
    assert_eq!(capabilities.domains()[0].max_queues(), max_queues);
    assert_eq!(capabilities.domains()[0].queue_depth(), depth);
    assert!(matches!(
        capabilities.publication_contract(),
        LogicalDevicePublicationContract::Discover { .. }
    ));
    assert_eq!(domain_slot_for_hardware_qid(1), Some(0));
    assert_eq!(domain_slot_for_hardware_qid(64), Some(63));
    assert_eq!(domain_slot_for_hardware_qid(0), None);
    assert_eq!(domain_slot_for_hardware_qid(65), None);
}

#[test]
fn msix_capabilities_freeze_each_vector_to_one_ownership_domain() {
    let identity = NonZeroUsize::new(0x2000).unwrap();
    let depth =
        HardwareQueueDepth::new(NonZeroU16::new(1).unwrap(), NonZeroU16::new(15).unwrap()).unwrap();

    let (capabilities, topology) = build_interrupt_capabilities(
        identity,
        u64::MAX,
        depth,
        NonZeroU16::new(4).unwrap(),
        NvmeInterruptMode::Msix,
        &[0, 1, 1, 3],
    )
    .unwrap();

    assert_eq!(topology.domains.len(), 3);
    assert_eq!(topology.domains[0].source.get(), 0);
    assert_eq!(topology.domains[0].global_queue_slots, [0]);
    assert_eq!(topology.domains[1].source.get(), 1);
    assert_eq!(topology.domains[1].global_queue_slots, [1, 2]);
    assert_eq!(topology.domains[2].source.get(), 3);
    assert_eq!(topology.domains[2].global_queue_slots, [3]);
    assert_eq!(capabilities.control_domain(), topology.domains[0].id);
    assert!(capabilities.domains()[0].is_required());
    assert!(!capabilities.domains()[1].is_required());
    assert!(!capabilities.domains()[2].is_required());
    assert_eq!(capabilities.domains()[0].min_queues().get(), 1);
    assert_eq!(capabilities.domains()[1].min_queues().get(), 2);
    assert_eq!(capabilities.domains()[1].max_queues().get(), 2);
    assert_eq!(capabilities.domains()[1].queue_depth().min(), depth.max());
    assert_eq!(capabilities.domains()[1].queue_depth().max(), depth.max());
}

#[test]
fn msix_activation_realizes_only_runtime_selected_optional_domains() {
    let identity = NonZeroUsize::new(0x3000).unwrap();
    let depth = HardwareQueueDepth::fixed(NonZeroU16::new(8).unwrap());
    let (capabilities, topology) = build_interrupt_capabilities(
        identity,
        u64::MAX,
        depth,
        NonZeroU16::new(4).unwrap(),
        NvmeInterruptMode::Msix,
        &[0, 1, 2, 3],
    )
    .unwrap();
    let selected_domain = capabilities.domains()[0].id();
    let plan = ActivationPlan::new(
        &capabilities,
        vec![DomainActivationPlan::new(
            selected_domain,
            NonZeroU16::MIN,
            depth.max(),
            capabilities.domains()[0].irq_sources(),
        )],
    )
    .unwrap();

    let selected = select_activation_topology(&topology, &plan).unwrap();

    assert_eq!(selected.queue_count, 1);
    assert_eq!(selected.domains.len(), 1);
    assert_eq!(selected.domains[0].source.get(), 0);
}

#[test]
fn ready_controller_without_namespace_publishes_no_block_device() {
    let (devices, routes) =
        build_namespace_publication("nvme", None, u64::MAX, 4096, None).unwrap();

    assert!(devices.is_empty());
    assert!(routes.is_empty());
}

#[test]
fn namespace_id_remains_the_stable_driver_device_key() {
    let namespace = Namespace {
        id: 37,
        lba_size: 512,
        lba_count: 1024,
        metadata_size: 0,
    };

    let (devices, routes) =
        build_namespace_publication("nvme", Some(namespace), u64::MAX, 4096, Some(128 * 1024))
            .unwrap();

    assert_eq!(devices.len(), 1);
    assert_eq!(routes.len(), 1);
    assert_eq!(devices[0].driver_key().get().get(), u64::from(namespace.id));
}
