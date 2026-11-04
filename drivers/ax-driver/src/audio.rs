//! Registered PCM cards, transferred to one task-context owner.

use alloc::{boxed::Box, vec::Vec};

use rdif_audio::PcmCard;
use rdrive::DriverGeneric;
#[cfg(all(
    feature = "virtio-sound",
    target_arch = "x86_64",
    target_pointer_width = "64"
))]
use rdrive::PlatformDevice;

struct PlatformPcmCard {
    card: Option<Box<dyn PcmCard>>,
}

impl DriverGeneric for PlatformPcmCard {
    fn name(&self) -> &str {
        self.card.as_deref().map_or("PCM card", |card| card.name())
    }
}

#[cfg(all(
    feature = "virtio-sound",
    target_arch = "x86_64",
    target_pointer_width = "64"
))]
pub(crate) fn register_pcm_card<T: PcmCard + 'static>(platform: PlatformDevice, card: T) {
    platform.register(PlatformPcmCard {
        card: Some(Box::new(card)),
    });
}

/// Moves every discovered PCM card out of the rdrive registry exactly once.
///
/// The registry wrapper remains available for device identity and borrow
/// tracking, but its card slot is empty after a successful transfer. Callers
/// must serialize this discovery so a card has one service-task owner.
pub fn take_pcm_cards() -> Vec<Box<dyn PcmCard>> {
    let mut cards = Vec::new();
    for device in rdrive::get_list::<PlatformPcmCard>() {
        let Ok(mut device) = device.lock() else {
            log::warn!("PCM card is busy and cannot be transferred");
            continue;
        };
        if let Some(card) = device.card.take() {
            cards.push(card);
        }
    }
    cards
}
