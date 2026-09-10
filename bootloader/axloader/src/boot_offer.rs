use httpboot_protocol::{BootArch, ImageFormat};

/// Returns whether an HTTP Boot kernel URL matches a supported server route.
///
/// The CI board server serves kernels under `/boot/sessions/<session>/kernel.elf`,
/// while the local QEMU smoke test serves `/kernel.elf` directly.
pub fn valid_kernel_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    if rest
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n' | b' ' | b'\t'))
    {
        return false;
    }

    let Some(path_start) = rest.find('/') else {
        return false;
    };
    let authority = &rest[..path_start];
    let path = &rest[path_start..];
    !authority.is_empty() && valid_kernel_path(path)
}

fn valid_kernel_path(path: &str) -> bool {
    if path == "/kernel.elf" {
        return true;
    }
    let Some(session_path) = path.strip_prefix("/boot/sessions/") else {
        return false;
    };
    let Some((session_id, image_path)) = session_path.split_once('/') else {
        return false;
    };
    valid_path_segment(session_id)
        && !image_path.is_empty()
        && image_path.split('/').all(valid_path_segment)
}

fn valid_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b' ' | b'\\' | b'?' | b'#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_qemu_and_board_kernel_urls() {
        assert!(valid_kernel_url("http://127.0.0.1/kernel.elf"));
        assert!(valid_kernel_url(
            "http://192.168.1.2:2999/boot/sessions/8d8a908f-a82e-4039-bea8-0fae30e50f42/kernel.elf"
        ));
        assert!(valid_kernel_url(
            "http://192.168.1.2:2999/boot/sessions/session-1/images/kernel%20debug.elf"
        ));
    }

    #[test]
    fn rejects_truncated_board_kernel_url() {
        assert!(!valid_kernel_url(
            "http://192.168.1.2:2999/b908f-a82e-4039-bea8-0fae30e50f42/kernel.elf"
        ));
    }

    #[test]
    fn rejects_malformed_kernel_urls() {
        assert!(!valid_kernel_url("https://192.168.1.2/kernel.elf"));
        assert!(!valid_kernel_url("http:///kernel.elf"));
        assert!(!valid_kernel_url(
            "http://192.168.1.2/boot/sessions//kernel.elf"
        ));
        assert!(!valid_kernel_url(
            "http://192.168.1.2/boot/sessions/session/../kernel.elf"
        ));
        assert!(!valid_kernel_url(
            "http://192.168.1.2/boot/sessions/session/"
        ));
    }

    #[test]
    fn rejects_wrong_kernel_metadata() {
        assert_eq!(
            validate_boot_manifest(
                BootManifest {
                    boot_id: "boot-1",
                    kernel_url: "http://10.77.0.1/kernel.elf",
                    kernel_size: 1,
                    kernel_sha256: &"x".repeat(64),
                    arch: BootArch::X86_64,
                    image_format: ImageFormat::Elf64,
                },
                None,
                BootArch::X86_64,
            ),
            BootManifestDecision::Reject
        );
    }

    #[test]
    fn does_not_retry_a_failed_boot_id() {
        assert_eq!(
            validate_boot_manifest(
                BootManifest {
                    boot_id: "boot-1",
                    kernel_url: "http://10.77.0.1/kernel.elf",
                    kernel_size: 1,
                    kernel_sha256: &"a".repeat(64),
                    arch: BootArch::X86_64,
                    image_format: ImageFormat::Elf64,
                },
                Some("boot-1"),
                BootArch::X86_64,
            ),
            BootManifestDecision::WaitForNewBoot
        );
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootManifestDecision {
    Accept,
    WaitForNewBoot,
    Reject,
}

#[derive(Debug, Clone, Copy)]
pub struct BootManifest<'a> {
    pub boot_id: &'a str,
    pub kernel_url: &'a str,
    pub kernel_size: u64,
    pub kernel_sha256: &'a str,
    pub arch: BootArch,
    pub image_format: ImageFormat,
}

pub fn validate_boot_manifest(
    manifest: BootManifest<'_>,
    failed_boot_id: Option<&str>,
    target_arch: BootArch,
) -> BootManifestDecision {
    if failed_boot_id == Some(manifest.boot_id) {
        return BootManifestDecision::WaitForNewBoot;
    }
    if manifest.boot_id.is_empty()
        || manifest.kernel_size == 0
        || !valid_sha256(manifest.kernel_sha256)
        || manifest.arch != target_arch
        || manifest.image_format != ImageFormat::Elf64
        || !valid_kernel_url(manifest.kernel_url)
    {
        return BootManifestDecision::Reject;
    }
    BootManifestDecision::Accept
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
