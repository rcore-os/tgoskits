pub(crate) mod board;
pub(crate) mod build;
pub(crate) mod case;
pub(crate) mod cross;
pub(crate) mod host_http;
pub(crate) mod pcm_input;
pub(crate) mod pcm_waveform;
pub(crate) mod qemu;
pub(crate) mod std;
pub(crate) mod suite;
pub(crate) mod timing;

pub(crate) const HOST_PCM_AUDIODEV_ID: &str = "pcm-capture";

pub(crate) fn qemu_has_audiodev(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "-audiodev" || arg.starts_with("-audiodev="))
}
