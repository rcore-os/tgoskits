use std::io::Write;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::*;
use crate::support::download::test_support;

fn sample_registry() -> &'static str {
    r#"
[[images]]
name = "linux"
version = "0.0.1"
released_at = "2025-01-01T00:00:00Z"
description = "Linux guest"
sha256 = "abc"
arch = "aarch64"
url = "https://example.com/linux-0.0.1.tar.gz"
"#
}

fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.finish().unwrap();
    }

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_data).unwrap();
    encoder.finish().unwrap()
}

fn make_tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
    let mut builder = tar::Builder::new(encoder);
    for (name, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, *contents).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn image_entry(name: &str, version: &str, sha256: &str, url: &str) -> ImageEntry {
    ImageEntry {
        name: name.to_string(),
        version: version.to_string(),
        released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
        description: "test image".to_string(),
        sha256: sha256.to_string(),
        arch: "aarch64".to_string(),
        url: url.to_string(),
    }
}

fn test_storage(root: &Path, image_registry: ImageRegistry) -> Storage {
    let download_dir = root.join("downloads");
    let extract_dir = root.join("extracted");
    fs::create_dir_all(&download_dir).unwrap();
    fs::create_dir_all(&extract_dir).unwrap();
    Storage {
        download_dir,
        extract_dir,
        image_registry,
    }
}

fn registry_url(image_registry: &ImageRegistry) -> test_support::MockHandle {
    test_support::register_text(
        "registry.toml",
        toml::to_string(image_registry).unwrap().into_bytes(),
    )
}

#[test]
fn names_use_registry_url_or_generated_fallback() {
    let xz_image = image_entry("linux", "0.0.1", "abc", "https://example.com/linux.tar.xz");
    assert_eq!(
        image_archive_filename(&xz_image, ImageSpecRef::parse("linux")),
        "linux.tar.xz"
    );

    let fallback_image = image_entry("linux", "0.0.1", "abc", "https://example.com/");
    assert_eq!(
        image_archive_filename(&fallback_image, ImageSpecRef::parse("linux")),
        "linux.tar.gz"
    );
    assert_eq!(
        image_archive_filename(&fallback_image, ImageSpecRef::parse("linux:0.0.1")),
        "linux-0.0.1.tar.gz"
    );
    assert_eq!(
        image_extract_dir_name(ImageSpecRef::parse("linux")),
        "linux"
    );
    assert_eq!(
        image_extract_dir_name(ImageSpecRef::parse("linux:0.0.1")),
        "linux-0.0.1"
    );
}

#[tokio::test]
async fn storage_fetches_registry_on_every_creation() {
    let registry_url = test_support::register_text("registry.toml", sample_registry().into());
    let root = tempdir().unwrap();
    let config = ImageConfig {
        registry: registry_url.url().to_string(),
        download_dir: root.path().join("downloads"),
        extract_dir: root.path().join("rootfs"),
    };

    Storage::new_from_config(&config).await.unwrap();
    let requests_after_first_creation = registry_url.request_count();
    Storage::new_from_config(&config).await.unwrap();

    assert!(registry_url.request_count() > requests_after_first_creation);
    assert!(config.download_dir.join(REGISTRY_FILENAME).is_file());
}

#[tokio::test]
async fn pull_image_uses_separate_download_and_extract_dirs() {
    let archive = make_tar_gz(&[("rootfs.img", b"rootfs"), ("qemu", b"kernel")]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("archive.tar.gz", archive);
    let root = tempdir().unwrap();
    let storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry(
                "demo-x86_64",
                "0.0.1",
                &sha256,
                archive_url.url(),
            )],
        },
    );

    let extracted = storage
        .pull_image(ImageSpecRef::parse("demo-x86_64"), true)
        .await
        .unwrap();

    assert_eq!(extracted, root.path().join("extracted/demo-x86_64"));
    assert_eq!(fs::read(extracted.join("rootfs.img")).unwrap(), b"rootfs");
    assert!(root.path().join("downloads/archive.tar.gz").is_file());
    assert!(!root.path().join("downloads/archive.tar.gz.part").exists());
}

#[tokio::test]
async fn unchanged_archive_preserves_modified_extracted_image_without_marker() {
    let archive = make_tar_gz(&[("kernel.bin", b"kernel")]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("linux.tar.gz", archive);
    let root = tempdir().unwrap();
    let storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry("linux", "0.0.1", &sha256, archive_url.url())],
        },
    );

    let extracted = storage
        .pull_image(ImageSpecRef::parse("linux"), true)
        .await
        .unwrap();
    fs::write(extracted.join("sentinel"), b"keep").unwrap();

    let extracted_again = storage
        .pull_image(ImageSpecRef::parse("linux"), true)
        .await
        .unwrap();

    assert_eq!(extracted_again, extracted);
    assert_eq!(fs::read(extracted.join("sentinel")).unwrap(), b"keep");
    assert_eq!(archive_url.request_count(), 1);
}

#[tokio::test]
async fn pull_rootfs_image_returns_direct_mutable_file() {
    let image_name = "rootfs-riscv64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
    let root = tempdir().unwrap();
    let storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry(image_name, "0.0.1", &sha256, archive_url.url())],
        },
    );

    let rootfs = storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();

    assert_eq!(rootfs, root.path().join("extracted").join(image_name));
    assert_eq!(fs::read(rootfs).unwrap(), b"rootfs");
    assert!(
        root.path()
            .join("downloads")
            .join(format!("{image_name}.tar.xz"))
            .is_file()
    );
}

#[tokio::test]
async fn unchanged_archive_preserves_modified_rootfs_without_marker() {
    let image_name = "rootfs-riscv64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
    let root = tempdir().unwrap();
    let storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry(image_name, "0.0.1", &sha256, archive_url.url())],
        },
    );

    let rootfs = storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();
    fs::write(&rootfs, b"patched rootfs").unwrap();
    let rootfs_again = storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();

    assert_eq!(rootfs_again, rootfs);
    assert_eq!(fs::read(rootfs_again).unwrap(), b"patched rootfs");
    assert_eq!(archive_url.request_count(), 1);
}

#[tokio::test]
async fn fixed_registry_preserves_modified_rootfs_across_storage_reloads() {
    let image_name = "rootfs-riscv64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
    let registry = ImageRegistry {
        images: vec![image_entry(
            image_name,
            "0.0.11",
            &sha256,
            archive_url.url(),
        )],
    };
    let registry_url = registry_url(&registry);
    let workspace = tempdir().unwrap();
    let config = ImageConfig {
        registry: registry_url.url().to_string(),
        download_dir: workspace.path().join("downloads"),
        extract_dir: workspace.path().join("rootfs"),
    };

    let storage = Storage::new_from_config(&config).await.unwrap();
    let rootfs = storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();
    fs::write(&rootfs, b"locally modified").unwrap();

    let reloaded_storage = Storage::new_from_config(&config).await.unwrap();
    let rootfs_again = reloaded_storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();

    assert_eq!(fs::read(rootfs_again).unwrap(), b"locally modified");
    assert_eq!(registry_url.request_count(), 2);
    assert_eq!(archive_url.request_count(), 1);
}

#[tokio::test]
async fn changed_archive_replaces_modified_rootfs() {
    let image_name = "rootfs-riscv64-alpine.img";
    let old_archive = make_tar_xz(&[(image_name, b"old rootfs")]);
    let old_sha256 = sha256_hex(&old_archive);
    let old_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), old_archive);
    let root = tempdir().unwrap();
    let old_storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry(image_name, "0.0.1", &old_sha256, old_url.url())],
        },
    );
    let rootfs = old_storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();
    fs::write(&rootfs, b"locally modified").unwrap();

    let new_archive = make_tar_xz(&[(image_name, b"new rootfs")]);
    let new_sha256 = sha256_hex(&new_archive);
    let new_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), new_archive);
    let new_storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry(image_name, "0.0.2", &new_sha256, new_url.url())],
        },
    );

    let updated_rootfs = new_storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();

    assert_eq!(fs::read(updated_rootfs).unwrap(), b"new rootfs");
    assert_eq!(new_url.request_count(), 1);
}

#[tokio::test]
async fn corrupt_archive_is_redownloaded_and_reextracted() {
    let archive = make_tar_gz(&[("rootfs.img", b"new-rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("linux.tar.gz", archive);
    let root = tempdir().unwrap();
    let storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry("linux", "0.0.1", &sha256, archive_url.url())],
        },
    );
    fs::write(root.path().join("downloads/linux.tar.gz"), b"corrupt").unwrap();
    let stale_extract_dir = root.path().join("extracted/linux");
    fs::create_dir_all(&stale_extract_dir).unwrap();
    fs::write(stale_extract_dir.join("rootfs.img"), b"old-rootfs").unwrap();

    let extracted = storage
        .pull_image(ImageSpecRef::parse("linux"), true)
        .await
        .unwrap();

    assert_eq!(
        fs::read(extracted.join("rootfs.img")).unwrap(),
        b"new-rootfs"
    );
}

#[tokio::test]
async fn failed_checksum_removes_downloaded_archive() {
    let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
    let archive_url = test_support::register_bytes("linux.tar.gz", archive);
    let root = tempdir().unwrap();
    let storage = test_storage(
        root.path(),
        ImageRegistry {
            images: vec![image_entry("linux", "0.0.1", "deadbeef", archive_url.url())],
        },
    );

    let err = storage
        .pull_image(ImageSpecRef::parse("linux"), false)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("checksum mismatch"));
    assert!(!root.path().join("downloads/linux.tar.gz").exists());
    assert!(!root.path().join("downloads/linux.tar.gz.part").exists());
}

#[test]
fn managed_rootfs_reference_resolves_to_configured_extract_dir() {
    let workspace = tempdir().unwrap();
    let image_name = "rootfs-aarch64-busybox.img";
    let config = ImageConfig {
        registry: "https://example.com/registry.toml".to_string(),
        download_dir: workspace.path().join("downloads"),
        extract_dir: workspace.path().join("working-rootfs"),
    };
    ImageConfig::write_config(workspace.path(), &config).unwrap();
    let reference = PathBuf::from(format!("${{workspace}}/tmp/axbuild/rootfs/{image_name}"));

    let resolved = resolve_managed_rootfs_path(workspace.path(), &reference).unwrap();

    assert_eq!(resolved, Some(config.extract_dir.join(image_name)));
}

#[tokio::test]
async fn ensure_rootfs_for_arch_uses_configured_direct_path() {
    let image_name = "rootfs-loongarch64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
    let registry = ImageRegistry {
        images: vec![image_entry(image_name, "0.0.1", &sha256, archive_url.url())],
    };
    let registry_url = registry_url(&registry);
    let workspace = tempdir().unwrap();
    let config = ImageConfig {
        registry: registry_url.url().to_string(),
        download_dir: workspace.path().join("downloads"),
        extract_dir: workspace.path().join("working-rootfs"),
    };
    ImageConfig::write_config(workspace.path(), &config).unwrap();

    let rootfs = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
        .await
        .unwrap();

    assert_eq!(rootfs, config.extract_dir.join(image_name));
    assert_eq!(fs::read(rootfs).unwrap(), b"rootfs");
}

#[tokio::test]
async fn ensure_managed_rootfs_accepts_locally_prepared_non_registry_image() {
    let registry_url = test_support::register_text("registry.toml", sample_registry().into());
    let workspace = tempdir().unwrap();
    let image_name = "rootfs-aarch64-pipuvapp.img";
    let config = ImageConfig {
        registry: registry_url.url().to_string(),
        download_dir: workspace.path().join("downloads"),
        extract_dir: workspace.path().join("working-rootfs"),
    };
    ImageConfig::write_config(workspace.path(), &config).unwrap();
    let reference = workspace.path().join("tmp/axbuild/rootfs").join(image_name);

    assert!(
        ensure_managed_rootfs(workspace.path(), "aarch64", &reference)
            .await
            .is_err()
    );

    let prepared = config.extract_dir.join(image_name);
    fs::create_dir_all(&config.extract_dir).unwrap();
    fs::write(&prepared, b"prepared-app-rootfs").unwrap();
    ensure_managed_rootfs(workspace.path(), "aarch64", &reference)
        .await
        .expect("locally prepared non-registry rootfs should be accepted");
}
