use std::io::Write;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::*;
use crate::{image::registry::RegistrySource, support::download::test_support};

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

fn image_entry(name: &str, version: &str, url: &str) -> ImageEntry {
    ImageEntry {
        name: name.to_string(),
        version: version.to_string(),
        released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
        description: "Linux guest".to_string(),
        sha256: "abc".to_string(),
        arch: "aarch64".to_string(),
        url: url.to_string(),
    }
}

#[test]
fn names_follow_registry_url_with_default_fallback() {
    let xz_image = image_entry("linux", "0.0.1", "https://example.com/linux.tar.xz");
    assert_eq!(
        image_archive_filename(&xz_image, ImageSpecRef::parse("linux")),
        "linux.tar.xz"
    );

    let fallback_image = image_entry("linux", "0.0.1", "https://example.com/");
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

#[test]
fn loads_local_registry() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(dir.path().join(REGISTRY_FILENAME), sample_registry()).unwrap();

    let storage = Storage::new(dir.path().to_path_buf()).unwrap();

    assert_eq!(storage.image_registry.images.len(), 1);
    assert_eq!(storage.image_registry.images[0].name, "linux");
}

#[tokio::test]
async fn auto_sync_fetches_registry_when_missing() {
    let dir = tempdir().unwrap();
    let sample = dir.path().join("sample.toml");
    fs::write(&sample, sample_registry()).unwrap();
    let image_registry = ImageRegistry::load_from_file(&sample).unwrap();

    let storage =
        Storage::new_with_auto_sync_for_test(dir.path().to_path_buf(), 60, image_registry)
            .await
            .unwrap();

    assert_eq!(storage.image_registry.images.len(), 1);
    assert!(dir.path().join(REGISTRY_FILENAME).exists());
}

#[tokio::test]
async fn pull_image_skips_reextract_when_marker_matches() {
    let dir = tempdir().unwrap();
    let image_name = "linux";
    let archive_bytes = make_tar_gz(&[("kernel.bin", b"kernel")]);
    let sha256 = sha256_hex(&archive_bytes);
    let registry_path = dir.path().join(REGISTRY_FILENAME);
    fs::write(
        &registry_path,
        format!(
            r#"
[[images]]
name = "{image_name}"
version = "0.0.1"
released_at = "2025-01-01T00:00:00Z"
description = "Linux guest"
sha256 = "{sha256}"
arch = "aarch64"
url = "https://example.com/{image_name}.tar.gz"
"#
        ),
    )
    .unwrap();
    fs::write(
        dir.path().join(image_archive_filename(
            &image_entry(
                image_name,
                "0.0.1",
                format!("https://example.com/{image_name}.tar.gz").as_str(),
            ),
            ImageSpecRef::parse(image_name),
        )),
        archive_bytes,
    )
    .unwrap();
    let extract_dir = dir
        .path()
        .join(image_extract_dir_name(ImageSpecRef::parse(image_name)));
    fs::create_dir_all(&extract_dir).unwrap();
    fs::write(extract_dir.join(EXTRACTED_SHA256_FILENAME), &sha256).unwrap();
    fs::write(extract_dir.join("sentinel"), b"keep").unwrap();

    let storage = Storage::new(dir.path().to_path_buf()).unwrap();
    let extracted = storage
        .pull_image(ImageSpecRef::parse(image_name), None, true)
        .await
        .unwrap();

    assert_eq!(extracted, extract_dir);
    assert_eq!(fs::read(extract_dir.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn config_without_auto_sync_requires_local_registry() {
    let dir = tempdir().unwrap();
    let config = ImageConfig {
        local_storage: dir.path().to_path_buf(),
        registry: "https://example.com/registry.toml".to_string(),
        auto_sync: false,
        auto_sync_threshold: 60,
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt.block_on(Storage::new_from_config(&config)).unwrap_err();

    assert!(err.to_string().contains("Failed to read image registry"));
}

#[test]
fn resolve_managed_rootfs_path_accepts_legacy_tmp_rootfs_reference() {
    let workspace = tempdir().unwrap();
    let image_name = "rootfs-aarch64-busybox.img";
    let config = ImageConfig {
        local_storage: workspace.path().join(".tgos-images"),
        registry: "https://example.com/registry.toml".to_string(),
        auto_sync: true,
        auto_sync_threshold: 60,
    };
    ImageConfig::write_config(workspace.path(), &config).unwrap();

    let legacy_path = workspace.path().join("tmp/axbuild/rootfs").join(image_name);
    let resolved = resolve_managed_rootfs_path(workspace.path(), &legacy_path).unwrap();

    assert_eq!(
        resolved,
        Some(config.local_storage.join(image_name).join(image_name))
    );
}

#[test]
fn resolve_managed_rootfs_path_accepts_workspace_legacy_reference() {
    let workspace = tempdir().unwrap();
    let image_name = "rootfs-aarch64-busybox.img";
    let config = ImageConfig {
        local_storage: workspace.path().join(".tgos-images"),
        registry: "https://example.com/registry.toml".to_string(),
        auto_sync: true,
        auto_sync_threshold: 60,
    };
    ImageConfig::write_config(workspace.path(), &config).unwrap();

    let legacy_path = PathBuf::from(format!("${{workspace}}/tmp/axbuild/rootfs/{image_name}"));
    let resolved = resolve_managed_rootfs_path(workspace.path(), &legacy_path).unwrap();

    assert_eq!(
        resolved,
        Some(config.local_storage.join(image_name).join(image_name))
    );
}

#[tokio::test]
async fn pull_downloads_and_extracts_image() {
    let archive = make_tar_gz(&[
        ("rootfs.img", b"rootfs"),
        ("qemu-aarch64", b"kernel"),
        ("axvm-bios.bin", b"bios"),
    ]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());

    let dir = tempdir().unwrap();
    let registry = ImageRegistry {
        images: vec![ImageEntry {
            name: "qemu_x86_64_nimbos".to_string(),
            version: "0.0.1".to_string(),
            released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
            description: "NimbOS guest".to_string(),
            sha256,
            arch: "x86_64".to_string(),
            url: archive_url.url().to_string(),
        }],
    };
    fs::write(
        dir.path().join(REGISTRY_FILENAME),
        toml::to_string(&registry).unwrap(),
    )
    .unwrap();

    let storage = Storage::new(dir.path().to_path_buf()).unwrap();
    let extracted = storage
        .pull_image(ImageSpecRef::parse("qemu_x86_64_nimbos"), None, true)
        .await
        .unwrap();

    assert_eq!(extracted, dir.path().join("qemu_x86_64_nimbos"));
    assert_eq!(fs::read(extracted.join("rootfs.img")).unwrap(), b"rootfs");
    assert!(dir.path().join("archive.tar.gz").exists());
    assert!(!dir.path().join("archive.tar.gz.part").exists());
}

#[tokio::test]
async fn pull_downloads_and_extracts_xz_image() {
    let archive = make_tar_xz(&[("rootfs.img", b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("rootfs.img.tar.xz", archive.clone());
    let dir = tempdir().unwrap();
    let storage = Storage {
        path: dir.path().to_path_buf(),
        image_registry: ImageRegistry {
            images: vec![ImageEntry {
                name: "rootfs-riscv64-alpine.img".to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Alpine rootfs".to_string(),
                sha256,
                arch: "riscv64".to_string(),
                url: archive_url.url().to_string(),
            }],
        },
    };

    let extracted = storage
        .pull_image(ImageSpecRef::parse("rootfs-riscv64-alpine.img"), None, true)
        .await
        .unwrap();

    assert_eq!(fs::read(extracted.join("rootfs.img")).unwrap(), b"rootfs");
    assert!(dir.path().join("rootfs.img.tar.xz").exists());
}

#[tokio::test]
async fn pull_rootfs_image_returns_extracted_rootfs_file() {
    let image_name = "rootfs-riscv64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
    let dir = tempdir().unwrap();
    let storage = Storage {
        path: dir.path().to_path_buf(),
        image_registry: ImageRegistry {
            images: vec![ImageEntry {
                name: image_name.to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Alpine rootfs".to_string(),
                sha256,
                arch: "riscv64".to_string(),
                url: archive_url.url().to_string(),
            }],
        },
    };

    let rootfs = storage
        .pull_rootfs_image(ImageSpecRef::parse(image_name))
        .await
        .unwrap();

    assert_eq!(rootfs, dir.path().join(image_name).join(image_name));
    assert_eq!(fs::read(rootfs).unwrap(), b"rootfs");
    assert_eq!(archive_url.request_count(), 1);
}

#[tokio::test]
async fn pull_rootfs_image_skips_download_when_archive_matches() {
    let image_name = "rootfs-riscv64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive.clone());
    let dir = tempdir().unwrap();
    let storage = Storage {
        path: dir.path().to_path_buf(),
        image_registry: ImageRegistry {
            images: vec![ImageEntry {
                name: image_name.to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Alpine rootfs".to_string(),
                sha256,
                arch: "riscv64".to_string(),
                url: archive_url.url().to_string(),
            }],
        },
    };

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
async fn ensure_rootfs_for_arch_uses_image_storage_path() {
    let image_name = "rootfs-loongarch64-alpine.img";
    let archive = make_tar_xz(&[(image_name, b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url =
        test_support::register_bytes(format!("{image_name}.tar.xz").as_str(), archive);
    let workspace = tempdir().unwrap();
    let config = ImageConfig {
        local_storage: workspace.path().join("image-cache"),
        registry: "https://example.com/registry.toml".to_string(),
        auto_sync: false,
        auto_sync_threshold: 60,
    };
    ImageConfig::write_config(workspace.path(), &config).unwrap();
    let registry = ImageRegistry {
        images: vec![ImageEntry {
            name: image_name.to_string(),
            version: "0.0.1".to_string(),
            released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
            description: "Alpine rootfs".to_string(),
            sha256,
            arch: "loongarch64".to_string(),
            url: archive_url.url().to_string(),
        }],
    };
    fs::create_dir_all(&config.local_storage).unwrap();
    fs::write(
        config.local_storage.join(REGISTRY_FILENAME),
        toml::to_string(&registry).unwrap(),
    )
    .unwrap();

    let rootfs = ensure_rootfs_for_arch(workspace.path(), "loongarch64")
        .await
        .unwrap();

    assert_eq!(
        rootfs,
        config.local_storage.join(image_name).join(image_name)
    );
    assert_eq!(fs::read(rootfs).unwrap(), b"rootfs");
}

#[tokio::test]
async fn pull_redownloads_when_existing_archive_is_invalid() {
    let archive = make_tar_gz(&[("rootfs.img", b"new-rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());
    let dir = tempdir().unwrap();
    let storage = Storage {
        path: dir.path().to_path_buf(),
        image_registry: ImageRegistry {
            images: vec![ImageEntry {
                name: "linux".to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Linux guest".to_string(),
                sha256,
                arch: "aarch64".to_string(),
                url: archive_url.url().to_string(),
            }],
        },
    };

    fs::write(dir.path().join("linux.tar.gz"), b"corrupt").unwrap();
    let extracted = storage
        .pull_image(ImageSpecRef::parse("linux"), None, true)
        .await
        .unwrap();

    assert_eq!(
        fs::read(extracted.join("rootfs.img")).unwrap(),
        b"new-rootfs"
    );
}

#[tokio::test]
async fn pull_uses_custom_output_dir() {
    let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
    let sha256 = sha256_hex(&archive);
    let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());
    let root = tempdir().unwrap();
    let output = root.path().join("images");
    let storage = Storage {
        path: root.path().join("default"),
        image_registry: ImageRegistry {
            images: vec![ImageEntry {
                name: "linux".to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Linux guest".to_string(),
                sha256,
                arch: "aarch64".to_string(),
                url: archive_url.url().to_string(),
            }],
        },
    };

    let extracted = storage
        .pull_image(ImageSpecRef::parse("linux"), Some(&output), true)
        .await
        .unwrap();

    assert_eq!(extracted, output.join("linux"));
    assert!(output.join("archive.tar.gz").exists());
    assert_eq!(
        fs::read(output.join("linux/rootfs.img")).unwrap(),
        b"rootfs"
    );
}

#[tokio::test]
async fn failed_checksum_does_not_leave_final_or_part_file() {
    let archive = make_tar_gz(&[("rootfs.img", b"rootfs")]);
    let archive_url = test_support::register_bytes("archive.tar.gz", archive.clone());
    let dir = tempdir().unwrap();
    let storage = Storage {
        path: dir.path().to_path_buf(),
        image_registry: ImageRegistry {
            images: vec![ImageEntry {
                name: "linux".to_string(),
                version: "0.0.1".to_string(),
                released_at: Some("2025-01-01T00:00:00Z".parse().unwrap()),
                description: "Linux guest".to_string(),
                sha256: "deadbeef".to_string(),
                arch: "aarch64".to_string(),
                url: archive_url.url().to_string(),
            }],
        },
    };

    let err = storage
        .pull_image(ImageSpecRef::parse("linux"), None, false)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("checksum mismatch"));
    assert!(!dir.path().join("linux.tar.gz").exists());
    assert!(!dir.path().join("linux.tar.gz.part").exists());
}

#[tokio::test]
async fn bootstrap_source_falls_back_when_default_is_unavailable() {
    let fallback_body = br#"
[[images]]
name = "linux"
version = "0.0.1"
description = "Linux guest"
sha256 = "abc"
arch = "aarch64"
    url = "https://example.com/linux.tar.gz"
    "#
    .to_vec();
    let fallback = test_support::register_text("fallback.toml", fallback_body);
    let client = http_client().unwrap();
    let source = ImageRegistry::resolve_bootstrap_source(
        &client,
        "mock://missing/default.toml",
        fallback.url(),
    )
    .await
    .unwrap();

    assert_eq!(
        source,
        RegistrySource {
            url: fallback.url().to_string(),
            kind: "fallback registry",
        }
    );
}

#[tokio::test]
async fn bootstrap_source_prefers_include_from_default() {
    let default_body = br#"
[[includes]]
url = "http://127.0.0.1:0/included.toml"
"#
    .to_vec();
    let default = test_support::register_text("default.toml", default_body);
    let client = http_client().unwrap();
    let source = ImageRegistry::resolve_bootstrap_source(
        &client,
        default.url(),
        "mock://missing/fallback.toml",
    )
    .await
    .unwrap();

    assert_eq!(source.kind, "included registry from default.toml");
    assert_eq!(source.url, "http://127.0.0.1:0/included.toml");
}
