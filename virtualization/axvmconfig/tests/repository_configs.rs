#[cfg(feature = "std")]
#[test]
fn every_repository_axvisor_vm_config_uses_the_typed_schema() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap();
    let config_root = workspace.join("os/axvisor/configs/vms");
    let mut configs = Vec::new();
    collect_toml_files(&config_root, &mut configs);
    assert!(!configs.is_empty());

    for path in configs {
        let source = std::fs::read_to_string(&path).unwrap();
        axvmconfig::AxVMCrateConfig::from_toml(&source)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

#[cfg(feature = "std")]
#[test]
fn every_architecture_template_uses_the_typed_schema() {
    let template_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let mut templates = Vec::new();
    collect_toml_files(&template_root, &mut templates);
    assert_eq!(templates.len(), 4);

    for path in templates {
        let source = std::fs::read_to_string(&path).unwrap();
        axvmconfig::AxVMCrateConfig::from_toml(&source)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

#[cfg(feature = "std")]
#[test]
fn x86_dma_passthrough_configs_keep_identity_ram_as_the_primary_region() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap();

    for file_name in ["linux-vmx-smp1.toml", "linux-svm-smp1.toml"] {
        let path = workspace
            .join("os/axvisor/configs/vms/qemu/x86_64")
            .join(file_name);
        let source = std::fs::read_to_string(&path).unwrap();
        let config = axvmconfig::AxVMCrateConfig::from_toml(&source).unwrap();
        let primary = config.memory.regions.first().unwrap();

        assert_eq!(
            config.kernel.kernel_load_addr,
            0x20_0000,
            "{}",
            path.display()
        );
        assert_eq!(primary.guest_base, 0, "{}", path.display());
        assert_eq!(primary.size, 0x800_0000, "{}", path.display());
        assert!(
            matches!(
                primary.backing,
                axvmconfig::MemoryBackingConfig::IdentityAllocate
            ),
            "{}",
            path.display()
        );
    }
}

#[cfg(feature = "std")]
fn collect_toml_files(directory: &std::path::Path, output: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_toml_files(&path, output);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            output.push(path);
        }
    }
}
