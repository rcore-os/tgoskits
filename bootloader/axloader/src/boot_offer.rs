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
    path == "/kernel.elf"
        || path
            .strip_prefix("/boot/sessions/")
            .and_then(|session_path| session_path.strip_suffix("/kernel.elf"))
            .is_some_and(|session| !session.is_empty() && !session.contains('/'))
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
            "http://192.168.1.2/boot/sessions/session/extra/kernel.elf"
        ));
        assert!(!valid_kernel_url(
            "http://192.168.1.2/boot/sessions/session/kernel.bin"
        ));
    }
}
