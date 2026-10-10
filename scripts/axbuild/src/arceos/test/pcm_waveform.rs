//! Host-side PCM output verification, not a guest success-marker substitute.
use std::{fs, path::Path};

use anyhow::{Context, bail, ensure};
use ostool::run::qemu::QemuConfig;
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    host_pcm_waveform: Option<WaveformConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaveformConfig {
    frames: usize,
}

pub(super) struct PcmWaveform {
    recording: tempfile::NamedTempFile,
    frames: usize,
}

impl PcmWaveform {
    /// Opt-in fixture for stereo S16LE/48-kHz PCM carrying the signed counter
    /// defined below. Each invocation gets a fresh recording under Cargo target.
    pub(super) fn start(
        config_path: &Path,
        target_dir: &Path,
        qemu: &mut QemuConfig,
    ) -> anyhow::Result<Option<Self>> {
        let config: Config = toml::from_str(&fs::read_to_string(config_path)?)?;
        let Some(config) = config.host_pcm_waveform else {
            return Ok(None);
        };
        ensure!(
            (1..=65_536).contains(&config.frames),
            "PCM fixture frames must be in 1..=65536"
        );
        ensure!(
            !qemu.args.iter().any(|arg| arg == "-audiodev"),
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
            format!(
                "wav,id=pcm-capture,path={path},out.frequency=48000,out.channels=2,out.format=s16"
            ),
        ]);
        Ok(Some(Self {
            recording,
            frames: config.frames,
        }))
    }

    pub(super) fn verify(self) -> anyhow::Result<()> {
        let bytes = fs::read(self.recording.path()).context("reading QEMU PCM recording")?;
        verify_waveform(&bytes, self.frames)?;
        println!(
            "PCM_WAVEFORM_OK frames={} bytes={}",
            self.frames,
            self.frames * 4
        );
        Ok(())
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
        "not a RIFF/WAVE recording"
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
            let pcm = wav.get(start..end).context("truncated WAV PCM chunk")?;
            ensure!(pcm.len().is_multiple_of(4), "incomplete PCM frame");
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
                .context("complete PCM waveform is missing, reordered or altered")?;
            ensure!(
                pcm[..position].iter().all(|&byte| byte == 0)
                    && pcm[position + expected.len()..]
                        .iter()
                        .all(|&byte| byte == 0),
                "non-silent output outside the expected PCM waveform"
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
    bail!("WAV recording contains no PCM data")
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
