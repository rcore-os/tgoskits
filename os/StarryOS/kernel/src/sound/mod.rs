//! StarryOS ALSA PCM service for the first VirtIO Sound profile.

pub(crate) mod config;
pub(crate) mod protocol;
mod service;
pub(crate) mod session;
mod worker;

use alloc::{boxed::Box, sync::Arc};
use core::time::Duration;

use ax_lazyinit::OnceLock;
use rdif_audio::PcmCard;

pub(crate) use self::{config::DirectionProfile, protocol::SoundError, service::SoundService};
use crate::{StarryError, StarryResult};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

static SERVICE: OnceLock<Arc<SoundService>> = OnceLock::new();

/// Starts the single PCM worker after driver registration and before devfs is
/// mounted. No card leaves the service unpublished, so devfs creates no node.
pub(crate) fn init() -> StarryResult<()> {
    if SERVICE.get().is_some() {
        return Ok(());
    }
    let mut cards = ax_driver::audio::take_pcm_cards();
    if cards.is_empty() {
        return Ok(());
    }
    if cards.len() != 1 {
        return Err(StarryError::OperationNotSupported);
    }
    let card: Box<dyn PcmCard> = cards.remove(0);
    let service = Arc::new(SoundService::new());
    let worker_service = service.clone();
    crate::task::kernel_thread_builder("pcm-service".into())
        .spawn(move || worker::run(card, worker_service))
        .map_err(|_| StarryError::Io)?;
    if !service.wait_for_discovery(DISCOVERY_TIMEOUT) {
        service.fail(protocol::SoundError::NoDevice);
        return Err(StarryError::TimedOut);
    }
    if let Some(error) = service.discovery_error() {
        return Err(error.into_starry());
    }
    if service.publication().is_none() {
        return Err(StarryError::NoSuchDevice);
    }
    let _ = SERVICE.call_once(|| service);
    Ok(())
}

pub(crate) fn service() -> Option<Arc<SoundService>> {
    SERVICE.get().cloned()
}
