// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

fn fallback_platform_for_arch(arch: &str) -> &'static str {
    match arch {
        "aarch64" => "aarch64-generic",
        "loongarch64" => "loongarch64-plat-dyn",
        "x86_64" => "dummy",
        "riscv64" => "riscv64-plat-dyn",
        _ => "dummy",
    }
}

fn asset_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn collect_assets(
    root: &Path,
    directory: &Path,
    assets: &mut Vec<(String, PathBuf)>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read web UI asset directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_assets(root, &path, assets)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("asset path is below dist")
            .to_string_lossy()
            .replace('\\', "/");
        let url = if relative == "index.html" {
            "/".to_string()
        } else {
            format!("/{relative}")
        };
        assets.push((url, path));
    }
    Ok(())
}

fn generate_ui_assets(out_dir: &Path) -> anyhow::Result<()> {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_WEB_UI");
    println!("cargo:rerun-if-changed=web-ui/dist");
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").context("CARGO_MANIFEST_DIR is not set")?);
    let dist = manifest_dir.join("web-ui/dist");
    if !dist.is_dir() {
        anyhow::bail!(
            "web-ui/dist is missing; run `cd web-ui && npm ci && npm run build` before enabling the web-ui feature"
        );
    }
    let mut assets = Vec::new();
    collect_assets(&dist, &dist, &mut assets)?;
    assets.sort_by(|left, right| left.0.cmp(&right.0));
    if !assets.iter().any(|(url, _)| url == "/") {
        anyhow::bail!("web-ui/dist does not contain index.html");
    }

    let mut generated = String::from("pub static UI_ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for (url, path) in assets {
        writeln!(
            generated,
            "    ({url:?}, {:?}, include_bytes!({:?})),",
            asset_content_type(&path),
            path.to_string_lossy()
        )?;
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("ui_assets.rs"), generated)
        .context("write generated web UI asset table")?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=linker.ld");
    let out_dir = PathBuf::from(env::var("OUT_DIR").context("OUT_DIR is not set")?);
    let linker = out_dir.join("linker.x");
    fs::write(&linker, include_str!("linker.ld"))?;
    println!("cargo:rustc-link-search={}", out_dir.display());
    fs::write(
        out_dir.join("../../..").join("linker.x"),
        include_str!("linker.ld"),
    )?;

    let arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").context("CARGO_CFG_TARGET_ARCH is not set")?;

    let platform = fallback_platform_for_arch(&arch);

    println!("cargo:rustc-cfg=platform=\"{platform}\"");

    if env::var_os("CARGO_FEATURE_WEB_UI").is_some() {
        generate_ui_assets(&out_dir)?;
    }

    Ok(())
}
