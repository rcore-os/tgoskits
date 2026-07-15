#[test]
fn axvm_uses_eoi_safe_wake_ipis_for_remote_vcpu_tasks() {
    let manifest = include_str!("../Cargo.toml");
    let ax_std_features = manifest
        .split_once("ax-std = { workspace = true, features = [")
        .and_then(|(_, tail)| tail.split_once("] }"))
        .map(|(features, _)| features)
        .expect("axvm must declare an explicit ax-std feature list");

    assert!(
        ax_std_features
            .lines()
            .any(|line| line.trim() == "\"wake-ipi\","),
        "AxVM must finish the host IPI EOI before a newly queued remote vCPU task can enter guest \
         context"
    );
    assert!(
        !ax_std_features
            .lines()
            .any(|line| line.trim() == "\"ipi\",")
    );
}
