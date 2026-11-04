//! Host-side PCM output verification, not a guest success-marker substitute.
use std::{fs, path::Path};

use anyhow::{Context, bail, ensure};
use ostool::run::qemu::QemuConfig;
use serde::Deserialize;

use crate::test::{HOST_PCM_AUDIODEV_ID, qemu_has_audiodev};

const WAVEFORM_OUTPUT_OPTIONS: &str = "out.frequency=48000,out.channels=2,out.format=s16";

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    host_pcm_waveform: Option<WaveformConfig>,
    #[serde(default)]
    host_pcm_input: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaveformConfig {
    frames: usize,
}

pub(crate) struct PcmWaveform {
    recording: tempfile::NamedTempFile,
    frames: usize,
}

impl PcmWaveform {
    /// Opt-in fixture for stereo S16LE/48-kHz PCM carrying the signed counter
    /// defined below. Each invocation gets a fresh recording under Cargo target.
    pub(crate) fn start(
        config_path: &Path,
        target_dir: &Path,
        qemu: &mut QemuConfig,
    ) -> anyhow::Result<Option<Self>> {
        let config: Config = toml::from_str(&fs::read_to_string(config_path)?)?;
        let Some(waveform) = config.host_pcm_waveform else {
            return Ok(None);
        };
        ensure!(
            config.host_pcm_input.is_none(),
            "host_pcm_waveform and host_pcm_input are mutually exclusive"
        );
        ensure!(
            (1..=65_536).contains(&waveform.frames),
            "PCM fixture frames must be in 1..=65536"
        );
        ensure!(
            !qemu_has_audiodev(&qemu.args),
            "PCM fixture owns -audiodev; do not configure a second backend"
        );
        fs::create_dir_all(target_dir)?;
        let recording = tempfile::Builder::new()
            .prefix("pcm-waveform-")
            .suffix(".wav")
            .tempfile_in(target_dir)?;
        let path = recording
            .path()
            .to_str()
            .context("PCM capture path is not UTF-8")?;
        ensure!(
            !path.contains(','),
            "PCM capture path cannot contain a QEMU option separator"
        );
        qemu.args.extend([
            "-audiodev".into(),
            format!("wav,id={HOST_PCM_AUDIODEV_ID},path={path},{WAVEFORM_OUTPUT_OPTIONS}"),
        ]);
        Ok(Some(Self {
            recording,
            frames: waveform.frames,
        }))
    }

    pub(crate) fn verify(self) -> anyhow::Result<()> {
        let frames = self.frames;
        let bytes = match fs::read(self.recording.path()).context("reading QEMU PCM recording") {
            Ok(bytes) => bytes,
            Err(error) => return self.retain_failed(error),
        };
        if let Err(error) = verify_waveform(&bytes, frames) {
            return self.retain_failed(error);
        }
        println!("PCM_WAVEFORM_OK frames={} bytes={}", frames, frames * 4);
        Ok(())
    }

    fn retain_failed(self, error: anyhow::Error) -> anyhow::Result<()> {
        let temporary_path = self.recording.path().to_path_buf();
        match self.recording.keep() {
            Ok((_file, path)) => {
                Err(error.context(format!("retained failed PCM WAV at `{}`", path.display())))
            }
            Err(persist) => Err(error.context(format!(
                "failed to retain PCM WAV at `{}`: {}",
                temporary_path.display(),
                persist.error
            ))),
        }
    }
}

fn expected_pcm(frames: usize) -> Vec<u8> {
    let mut expected = Vec::with_capacity(frames * 4);
    for index in 0..frames {
        let left = ((index * 97) % 20001) as i16 - 10000;
        expected.extend_from_slice(&left.to_le_bytes());
        expected.extend_from_slice(&(-left).to_le_bytes());
    }
    expected
}

fn verify_waveform(wav: &[u8], frames: usize) -> anyhow::Result<()> {
    ensure!(
        wav.len() >= 12 && &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE",
        "not a RIFF/WAVE recording (file_bytes={})",
        wav.len()
    );
    let mut offset = 12usize;
    let mut format_valid = false;
    while offset + 8 <= wav.len() {
        let id = &wav[offset..offset + 4];
        let length = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        if id == b"data" {
            ensure!(format_valid, "PCM format must precede data");
            // The runner stops QEMU after its guest marker. QEMU's WAV backend
            // may not finalize the zero data-length placeholder before SIGKILL.
            // Validate all physically written bytes, not that unfinalized header.
            let end = if length == 0 {
                wav.len()
            } else {
                start
                    .checked_add(length)
                    .context("WAV data length overflow")?
            };
            let pcm = wav.get(start..end).with_context(|| {
                format!(
                    "truncated WAV PCM chunk (data_start={start}, declared_data_bytes={length}, \
                     available_pcm_bytes={}, available_frames={}, expected_frames={frames})",
                    wav.len().saturating_sub(start),
                    wav.len().saturating_sub(start) / 4
                )
            })?;
            ensure!(
                pcm.len().is_multiple_of(4),
                "incomplete PCM frame (actual_pcm_bytes={}, complete_frames={})",
                pcm.len(),
                pcm.len() / 4
            );
            let actual_frames = pcm.len() / 4;
            let expected = expected_pcm(frames);
            let position = pcm
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .find_map(|(index, _)| {
                    let position = index * 4;
                    (pcm.get(position..position + expected.len()) == Some(expected.as_slice()))
                        .then_some(position)
                })
                .with_context(|| {
                    format!(
                        "complete PCM waveform is missing, reordered or altered ({})",
                        waveform_mismatch_detail(pcm, &expected, frames, actual_frames)
                    )
                })?;
            let prefix_nonzero = first_nonzero_frame(&pcm[..position]);
            let suffix_start = position + expected.len();
            let suffix_nonzero =
                first_nonzero_frame(&pcm[suffix_start..]).map(|frame| suffix_start / 4 + frame);
            ensure!(
                prefix_nonzero.is_none() && suffix_nonzero.is_none(),
                "non-silent output outside the expected PCM waveform (actual_pcm_bytes={}, \
                 actual_frames={actual_frames}, expected_frames={frames}, \
                 waveform_start_frame={}, leading_nonzero_frame={prefix_nonzero:?}, \
                 trailing_nonzero_frame={suffix_nonzero:?})",
                pcm.len(),
                position / 4
            );
            ensure!(
                end == wav.len(),
                "unexpected chunks or trailing bytes after PCM data"
            );
            return Ok(());
        }
        let end = start.checked_add(length).context("WAV chunk overflow")?;
        let chunk = wav.get(start..end).context("truncated WAV chunk")?;
        if id == b"fmt " {
            ensure!(
                !format_valid && chunk.len() >= 16,
                "missing or duplicate WAV format"
            );
            let u16_at = |offset| u16::from_le_bytes([chunk[offset], chunk[offset + 1]]);
            let u32_at = |offset| {
                u32::from_le_bytes(
                    chunk[offset..offset + 4]
                        .try_into()
                        .expect("checked WAV format extent"),
                )
            };
            ensure!(
                u16_at(0) == 1
                    && u16_at(2) == 2
                    && u32_at(4) == 48000
                    && u32_at(8) == 192000
                    && u16_at(12) == 4
                    && u16_at(14) == 16,
                "expected stereo S16LE 48-kHz WAV"
            );
            format_valid = true;
        }
        offset = end
            .checked_add(length % 2)
            .context("WAV padding overflow")?;
    }
    bail!(
        "WAV recording contains no PCM data (file_bytes={}, expected_frames={frames})",
        wav.len()
    )
}

fn waveform_mismatch_detail(
    pcm: &[u8],
    expected: &[u8],
    expected_frames: usize,
    actual_frames: usize,
) -> String {
    let signal_start_frame = first_nonzero_frame(pcm);
    let signal_start = signal_start_frame
        .map(|frame| frame * 4)
        .unwrap_or(pcm.len());
    let signal = pcm.get(signal_start..).unwrap_or_default();
    let common_prefix_bytes = signal
        .iter()
        .zip(expected)
        .take_while(|(actual, expected)| actual == expected)
        .count();
    let common_prefix_frames = common_prefix_bytes / 4;
    let first_difference_frame =
        signal_start_frame.map_or(actual_frames, |frame| frame + common_prefix_frames);
    format!(
        "actual_pcm_bytes={}, actual_frames={actual_frames}, expected_frames={expected_frames}, \
         expected_pcm_bytes={}, signal_start_frame={signal_start_frame:?}, \
         signal_common_prefix_bytes={common_prefix_bytes}, \
         signal_common_prefix_frames={common_prefix_frames}, \
         first_difference_frame={first_difference_frame}, first_difference_byte={}, \
         actual_frame={}, expected_frame={}",
        pcm.len(),
        expected.len(),
        signal_start + common_prefix_bytes,
        frame_hex(pcm, first_difference_frame),
        frame_hex(expected, common_prefix_frames)
    )
}

fn first_nonzero_frame(pcm: &[u8]) -> Option<usize> {
    pcm.as_chunks::<4>()
        .0
        .iter()
        .position(|frame| frame.iter().any(|&byte| byte != 0))
}

fn frame_hex(pcm: &[u8], frame_index: usize) -> String {
    let frame = frame_index
        .checked_mul(4)
        .and_then(|start| start.checked_add(4).map(|end| (start, end)))
        .and_then(|(start, end)| pcm.get(start..end))
        .and_then(|frame| <[u8; 4]>::try_from(frame).ok());
    match frame {
        Some(frame) => frame.iter().map(|&byte| format!("{byte:02x}")).collect(),
        None => "none".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording(pcm: &[u8]) -> Vec<u8> {
        let mut wav = Vec::from(*b"RIFF\0\0\0\0WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&192000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data\0\0\0\0");
        wav.extend_from_slice(pcm);
        wav
    }

    #[test]
    fn waveform_checks_real_samples_and_rejects_corruption_or_false_success() {
        let expected = expected_pcm(32);
        let mut pcm = vec![0; 16];
        pcm.extend_from_slice(&expected);
        pcm.extend_from_slice(&[0; 16]);
        let wav = recording(&pcm);
        assert!(verify_waveform(&wav, 32).is_ok());
        let mut corrupted = wav.clone();
        corrupted[44 + 16 + 9] ^= 1;
        assert!(verify_waveform(&corrupted, 32).is_err());
        assert!(verify_waveform(&recording(&[0; 256]), 32).is_err());
        assert!(verify_waveform(&recording(&expected[..expected.len() - 4]), 32).is_err());
        let mut repeated = expected.clone();
        repeated.extend_from_slice(&expected);
        assert!(verify_waveform(&recording(&repeated), 32).is_err());
        let mut wrong_format = wav.clone();
        wrong_format[24..28].copy_from_slice(&44100u32.to_le_bytes());
        assert!(verify_waveform(&wrong_format, 32).is_err());
        for length in 0..44 {
            assert!(verify_waveform(&wav[..length], 32).is_err());
        }
    }
}
