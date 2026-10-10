#![no_std]
#![no_main]

extern crate alloc;
extern crate ax_std as std;

use alloc::{vec, vec::Vec};
use core::time::Duration;

use ax_driver::playback::{Playback, PlaybackError, PlaybackToken};

const PERIODS: usize = 4;
const TIMEOUT_NS: u64 = 3_000_000_000;

fn collect(device: &mut dyn Playback, completed: &mut Vec<PlaybackToken>, deadline: u64) {
    match device.complete().expect("PCM device completion failed") {
        Some(token) => completed.push(token),
        None => {
            assert!(
                ax_hal::time::monotonic_time_nanos() < deadline,
                "PCM completion timed out"
            );
            ax_hal::time::busy_wait(Duration::from_micros(100));
        }
    }
}

fn play(device: &mut dyn Playback) {
    let config = *device
        .configurations()
        .first()
        .expect("no supported PCM configuration");
    assert_eq!(
        (
            config.sample_rate_hz,
            config.channels,
            config.period_frames,
            config.period_count
        ),
        (48_000, 2, 1024, 4)
    );
    assert!(device.max_poll_interval_ns() >= 100_000);
    let mut unsupported = config;
    unsupported.sample_rate_hz = 44_100;
    assert_eq!(device.prepare(unsupported), Err(PlaybackError::Unsupported));
    device.prepare(config).expect("PCM prepare failed");
    assert_eq!(device.submit(&[]), Err(PlaybackError::InvalidPeriod));

    let period_bytes = config
        .period_bytes()
        .expect("invalid advertised PCM configuration");
    let mut pcm = vec![0; period_bytes];
    let mut submitted = Vec::new();
    let mut completed = Vec::new();
    let deadline = ax_hal::time::monotonic_time_nanos().saturating_add(TIMEOUT_NS);
    for period in 0..PERIODS {
        for (frame, samples) in pcm.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let index = period * config.period_frames as usize + frame;
            // The WAV fixture uses this signed, channel-distinguishing sequence
            // to detect missing, duplicated, reordered or altered samples.
            let left = ((index * 97) % 20001) as i16 - 10000;
            samples[..2].copy_from_slice(&left.to_le_bytes());
            samples[2..].copy_from_slice(&(-left).to_le_bytes());
        }
        loop {
            match device.submit(&pcm) {
                Ok(token) => {
                    submitted.push(token);
                    break;
                }
                Err(PlaybackError::Again) => collect(device, &mut completed, deadline),
                Err(error) => panic!("PCM submit failed: {error}"),
            }
        }
        // The public submission contract copies caller memory before success.
        pcm.fill(0);
    }
    while completed.len() < submitted.len() {
        collect(device, &mut completed, deadline);
    }
    assert_eq!(
        completed, submitted,
        "PCM completion order differs from submission order"
    );
    device.release().expect("PCM output drain failed");

    // Exercise cancellation through the same public capability, without adding
    // a second payload to the captured waveform.
    device.prepare(config).unwrap();
    device.submit(&pcm).unwrap();
    device.abort().expect("PCM cancellation failed");
    assert_eq!(
        device.complete().unwrap(),
        None,
        "cancelled token was published as completed"
    );
    device.prepare(config).unwrap();
    device.release().unwrap();
    device.shutdown().expect("PCM shutdown failed");
    assert_eq!(device.prepare(config), Err(PlaybackError::BadState));
}

#[unsafe(no_mangle)]
fn main() {
    let mut devices =
        ax_driver::playback::take_playback_devices().expect("PCM ownership transfer failed");
    assert_eq!(
        devices.len(),
        1,
        "QEMU must discover one registered PCM device"
    );
    let device = &mut devices[0];
    std::println!("PCM_PLAYBACK_DEVICE {}", device.name());
    play(device.as_mut());
    std::println!("INTEL_HDA_QEMU_OK");
    std::process::exit(0);
}
