// Copyright 2025 The tgoskits Team
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

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::arceos::config::{ArceosConfig, Arch, NetDev};

/// QEMU runner
pub struct QemuRunner {
    config: ArceosConfig,
    image_path: PathBuf,
    arceos_dir: PathBuf,
}

impl QemuRunner {
    pub fn new(config: ArceosConfig, image_path: PathBuf, arceos_dir: PathBuf) -> Self {
        Self {
            config,
            image_path,
            arceos_dir,
        }
    }

    /// Build QEMU command arguments
    pub fn build_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        // Machine and CPU
        args.push("-machine".to_string());
        args.push(self.machine());
        args.push("-cpu".to_string());
        args.push(self.cpu());

        // Kernel image
        args.push("-kernel".to_string());
        args.push(self.image_path.display().to_string());

        // Memory
        let mem = self.config.mem.as_deref().unwrap_or("128M");
        args.push("-m".to_string());
        args.push(mem.to_string());

        // SMP
        let smp = self.config.smp.unwrap_or(1);
        args.push("-smp".to_string());
        args.push(smp.to_string());

        // Block device
        if self.config.qemu.blk {
            args.push("-device".to_string());
            args.push("virtio-blk-pci,drive=disk0".to_string());
            if let Some(disk_img) = &self.config.qemu.disk_image {
                args.push("-drive".to_string());
                args.push(format!(
                    "id=disk0,if=none,format=raw,file={}",
                    disk_img.display()
                ));
            } else {
                // Use default disk image
                let default_disk = self.arceos_dir.join("resources/disk.img");
                if default_disk.exists() {
                    args.push("-drive".to_string());
                    args.push(format!(
                        "id=disk0,if=none,format=raw,file={}",
                        default_disk.display()
                    ));
                }
            }
        }

        // Network
        if self.config.qemu.net {
            args.push("-device".to_string());
            args.push("virtio-net-pci,netdev=net0".to_string());

            match self.config.qemu.net_dev {
                NetDev::User => {
                    args.push("-netdev".to_string());
                    args.push("user,id=net0,hostfwd=tcp::5555-:5555".to_string());
                }
                NetDev::Tap => {
                    args.push("-netdev".to_string());
                    args.push("tap,id=net0,script=no".to_string());
                }
                NetDev::Bridge => {
                    args.push("-netdev".to_string());
                    args.push("bridge,id=net0,br=virbr0".to_string());
                }
            }
        }

        // Graphic
        if self.config.qemu.graphic {
            args.push("-device".to_string());
            args.push("virtio-gpu-pci".to_string());
            args.push("-display".to_string());
            args.push("gtk".to_string());
        } else {
            args.push("-nographic".to_string());
            args.push("-serial".to_string());
            args.push("mon:stdio".to_string());
        }

        // Acceleration
        if self.config.qemu.accel {
            match self.config.arch {
                Arch::X86_64 => {
                    args.push("-accel".to_string());
                    args.push("kvm".to_string());
                }
                Arch::AArch64 => {
                    args.push("-accel".to_string());
                    args.push("hvf".to_string());
                }
                _ => {}
            }
        }

        // Extra args
        args.extend(self.config.qemu.extra_args.iter().cloned());

        args
    }

    /// Get machine type
    fn machine(&self) -> String {
        self.config.arch.to_qemu_machine().to_string()
    }

    /// Get CPU type
    fn cpu(&self) -> String {
        match self.config.arch {
            Arch::X86_64 => "max".to_string(),
            Arch::AArch64 => "cortex-a72".to_string(),
            Arch::RiscV64 => "rv64".to_string(),
            Arch::LoongArch64 => "la464".to_string(),
        }
    }

    /// Get QEMU binary name
    pub fn qemu_binary(&self) -> String {
        format!("qemu-system-{}", self.config.arch.to_qemu_arch())
    }

    /// Run QEMU
    pub async fn run(&self) -> Result<()> {
        let qemu = self.qemu_binary();
        let args = self.build_args();

        tracing::info!("Running QEMU: {} {}", qemu, args.join(" "));

        #[cfg(feature = "tokio")]
        {
            let status = tokio::process::Command::new(&qemu)
                .args(&args)
                .status()
                .await
                .with_context(|| format!("Failed to run {}", qemu))?;

            if !status.success() {
                anyhow::bail!("QEMU exited with status: {}", status);
            }
        }

        #[cfg(not(feature = "tokio"))]
        {
            let status = Command::new(&qemu)
                .args(&args)
                .status()
                .with_context(|| format!("Failed to run {}", qemu))?;

            if !status.success() {
                anyhow::bail!("QEMU exited with status: {}", status);
            }
        }

        Ok(())
    }

    /// Get QEMU command as a string (for debugging)
    pub fn command_string(&self) -> String {
        let qemu = self.qemu_binary();
        let args = self.build_args();
        format!("{} {}", qemu, args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::arceos::config::{ArceosConfig, QemuOptions};

    #[test]
    fn test_qemu_binary() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));
        assert_eq!(runner.qemu_binary(), "qemu-system-aarch64");
    }

    #[test]
    fn test_cpu_type() {
        let config = ArceosConfig {
            arch: Arch::X86_64,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-cpu" && w[1] == "max"));
    }

    #[test]
    fn test_memory_default() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            mem: None,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "128M"));
    }

    #[test]
    fn test_memory_custom() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            mem: Some("256M".to_string()),
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "256M"));
    }

    #[test]
    fn test_network_user() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            qemu: QemuOptions {
                net: true,
                net_dev: NetDev::User,
                ..Default::default()
            },
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.iter().any(|a| a.contains("user")));
    }
}
