use axdevice::{PciCapabilityEffectAccess, PciConfigEffectId};
use axvirtio_common::pci::VirtioPciCapabilityType;

use super::{super::config::decode_pci_cfg_bytes, *};

#[test]
fn conversion_preserves_derived_lengths_and_pci_cfg_effect() {
    let capabilities = VirtioPciCapabilitySet::new(16);
    let specs = virtio_capabilities(&capabilities).unwrap();
    assert_eq!(specs.len(), 5);

    let expected = [
        (VirtioPciCapabilityType::Common, 0, 0x000, 0x38, 0),
        (VirtioPciCapabilityType::Notify, 0, 0x100, 0x04, 4),
        (VirtioPciCapabilityType::Isr, 0, 0x200, 0x01, 0),
        (VirtioPciCapabilityType::Device, 0, 0x300, 16, 0),
        (VirtioPciCapabilityType::PciConfig, 0, 0, 0, 0),
    ];
    for ((capability, spec), (cfg_type, bar, offset, length, multiplier)) in
        capabilities.as_slice().iter().zip(&specs).zip(expected)
    {
        assert_eq!(capability.cfg_type(), cfg_type);
        assert_eq!(capability.bar(), bar);
        assert_eq!(capability.offset(), offset);
        assert_eq!(capability.length(), length);
        assert_eq!(capability.notify_off_multiplier(), multiplier);

        let body = spec.body();
        assert_eq!(body.len(), usize::from(capability.serialized_length()) - 2);
        assert_eq!(body[0], capability.serialized_length());
        assert_eq!(body[1], cfg_type as u8);
        assert_eq!(body[2], bar);
        assert_eq!(u32::from_le_bytes(body[6..10].try_into().unwrap()), offset);
        assert_eq!(u32::from_le_bytes(body[10..14].try_into().unwrap()), length);
        if body.len() >= 18 {
            assert_eq!(
                u32::from_le_bytes(body[14..18].try_into().unwrap()),
                multiplier
            );
        }
    }

    assert_eq!(specs[4].effects().len(), 1);
    let effect = specs[4].effects()[0];
    assert_eq!(effect.effect(), PciConfigEffectId::new(1));
    assert_eq!(effect.offset(), 16);
    assert_eq!(effect.length(), 4);
    assert_eq!(effect.access(), PciCapabilityEffectAccess::ReadWrite);
    assert_eq!(specs[4].write_mask()[2], u8::MAX);
    assert!(
        specs[4].write_mask()[6..14]
            .iter()
            .all(|mask| *mask == u8::MAX)
    );
    assert!(specs[4].write_mask()[14..].iter().all(|mask| *mask == 0));
}

#[test]
fn pci_cfg_selector_targets_bar_zero_without_including_access_width() {
    let mut body = [0; 18];
    body[0] = 20;
    body[1] = VirtioPciCapabilityType::PciConfig as u8;
    body[2] = 0;
    body[6..10].copy_from_slice(&0x2f0_u32.to_le_bytes());
    body[10..14].copy_from_slice(&4_u32.to_le_bytes());

    assert_eq!(
        decode_pci_cfg_bytes(PciConfigEffectId::new(1), 16, AccessWidth::Dword, &body,),
        Ok(0x2f0)
    );

    body[10..14].copy_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        decode_pci_cfg_bytes(PciConfigEffectId::new(1), 17, AccessWidth::Word, &body,),
        Ok(0x2f1)
    );

    body[10..14].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        decode_pci_cfg_bytes(PciConfigEffectId::new(1), 18, AccessWidth::Byte, &body,),
        Ok(0x2f2)
    );
}

#[test]
fn pci_cfg_selector_rejects_wrong_bar_width_and_boundary() {
    let mut body = [0; 18];
    body[0] = 20;
    body[1] = VirtioPciCapabilityType::PciConfig as u8;
    body[2] = 1;
    body[6..10].copy_from_slice(&0x0_u32.to_le_bytes());
    body[10..14].copy_from_slice(&1_u32.to_le_bytes());
    assert!(decode_pci_cfg_bytes(PciConfigEffectId::new(1), 16, AccessWidth::Byte, &body).is_err());

    body[2] = 0;
    body[6..10].copy_from_slice(&0xfff_u32.to_le_bytes());
    body[10..14].copy_from_slice(&4_u32.to_le_bytes());
    assert!(
        decode_pci_cfg_bytes(PciConfigEffectId::new(1), 16, AccessWidth::Dword, &body,).is_err()
    );
}
