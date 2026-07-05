use anyhow::anyhow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtualizationBackend {
    Vmx,
    Svm,
}

impl VirtualizationBackend {
    fn feature(self) -> &'static str {
        match self {
            Self::Vmx => "vmx",
            Self::Svm => "svm",
        }
    }

    fn from_feature(feature: &str) -> Option<Self> {
        match feature {
            "vmx" | "axvm/vmx" => Some(Self::Vmx),
            "svm" | "axvm/svm" => Some(Self::Svm),
            _ => None,
        }
    }
}

pub(super) fn normalize_backend_features(features: &mut Vec<String>) -> anyhow::Result<()> {
    normalize_backend_features_with(features, detect_host_backend)
}

fn normalize_backend_features_with(
    features: &mut Vec<String>,
    detect_backend: impl FnOnce() -> anyhow::Result<VirtualizationBackend>,
) -> anyhow::Result<()> {
    let has_vmx = features.iter().any(|feature| {
        VirtualizationBackend::from_feature(feature) == Some(VirtualizationBackend::Vmx)
    });
    let has_svm = features.iter().any(|feature| {
        VirtualizationBackend::from_feature(feature) == Some(VirtualizationBackend::Svm)
    });

    match (has_vmx, has_svm) {
        (true, true) => Err(anyhow!(
            "x86_64 Axvisor features `vmx` and `svm` are mutually exclusive"
        )),
        (true, false) | (false, true) => Ok(()),
        (false, false) => {
            let backend = detect_backend()?;
            println!(
                "Auto-selected x86_64 virtualization backend: {}",
                backend.feature()
            );
            features.push(backend.feature().to_string());
            Ok(())
        }
    }
}

fn detect_host_backend() -> anyhow::Result<VirtualizationBackend> {
    if let Ok(value) = std::env::var("AXVISOR_X86_BACKEND") {
        return parse_backend(&value);
    }

    detect_host_backend_from_cpuid()
}

fn parse_backend(value: &str) -> anyhow::Result<VirtualizationBackend> {
    match value.trim().to_ascii_lowercase().as_str() {
        "vmx" | "intel" => Ok(VirtualizationBackend::Vmx),
        "svm" | "amd" => Ok(VirtualizationBackend::Svm),
        other => Err(anyhow!(
            "invalid AXVISOR_X86_BACKEND value `{other}`; expected `vmx`/`intel` or `svm`/`amd`"
        )),
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detect_host_backend_from_cpuid() -> anyhow::Result<VirtualizationBackend> {
    let cpuid = raw_cpuid::CpuId::new();
    let vendor = cpuid
        .get_vendor_info()
        .ok_or_else(|| anyhow!("failed to read x86 CPUID vendor information"))?;

    match vendor.as_str() {
        "GenuineIntel" => Ok(VirtualizationBackend::Vmx),
        "AuthenticAMD" => Ok(VirtualizationBackend::Svm),
        _ => Err(anyhow!(
            "unsupported x86 CPU vendor `{vendor}` for automatic Axvisor backend selection; set \
             AXVISOR_X86_BACKEND=vmx or AXVISOR_X86_BACKEND=svm to override"
        )),
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn detect_host_backend_from_cpuid() -> anyhow::Result<VirtualizationBackend> {
    Err(anyhow!(
        "cannot auto-select x86_64 Axvisor virtualization backend on non-x86 host; set \
         AXVISOR_X86_BACKEND=vmx or AXVISOR_X86_BACKEND=svm"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_auto_selects_vmx_when_missing() {
        let mut features = vec!["fs".to_string()];

        normalize_backend_features_with(&mut features, || Ok(VirtualizationBackend::Vmx)).unwrap();

        assert!(features.contains(&"vmx".to_string()));
        assert!(!features.contains(&"svm".to_string()));
    }

    #[test]
    fn backend_auto_selects_svm_when_missing() {
        let mut features = vec!["fs".to_string()];

        normalize_backend_features_with(&mut features, || Ok(VirtualizationBackend::Svm)).unwrap();

        assert!(features.contains(&"svm".to_string()));
        assert!(!features.contains(&"vmx".to_string()));
    }

    #[test]
    fn backend_keeps_explicit_choice() {
        let mut features = vec!["svm".to_string()];

        normalize_backend_features_with(&mut features, || Ok(VirtualizationBackend::Vmx)).unwrap();

        assert!(features.contains(&"svm".to_string()));
        assert!(!features.contains(&"vmx".to_string()));
    }

    #[test]
    fn backend_keeps_nested_axvm_choice() {
        let mut features = vec!["axvm/svm".to_string()];

        normalize_backend_features_with(&mut features, || Ok(VirtualizationBackend::Vmx)).unwrap();

        assert!(features.contains(&"axvm/svm".to_string()));
        assert!(!features.contains(&"vmx".to_string()));
    }

    #[test]
    fn backend_rejects_conflicting_features() {
        let mut features = vec!["vmx".to_string(), "svm".to_string()];

        let err = normalize_backend_features_with(&mut features, || Ok(VirtualizationBackend::Vmx))
            .unwrap_err();

        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn backend_rejects_conflicting_nested_features() {
        let mut features = vec!["vmx".to_string(), "axvm/svm".to_string()];

        let err = normalize_backend_features_with(&mut features, || Ok(VirtualizationBackend::Vmx))
            .unwrap_err();

        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn parses_backend_override() {
        assert_eq!(parse_backend("intel").unwrap(), VirtualizationBackend::Vmx);
        assert_eq!(parse_backend("svm").unwrap(), VirtualizationBackend::Svm);
        assert!(parse_backend("unknown").is_err());
    }
}
