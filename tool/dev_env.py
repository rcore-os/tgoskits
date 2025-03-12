#!/usr/bin/env python3
import os
import subprocess
import argparse


def main():
    # 创建 ArgumentParser 对象
    parser = argparse.ArgumentParser(description="Development environment setup script")

    parser.add_argument(
        "--repo",
        type=str,
        default="git@github.com:arceos-hypervisor",
        help="Specify the repo to use",
    )

    # 解析命令行参数
    args = parser.parse_args()

    repo = args.repo

    # 使用 branch 参数
    print(f"Using: {repo} ")

    # 创建 crates 目录
    os.makedirs("crates", exist_ok=True)

    # 克隆其他仓库到 crates 目录
    repos = [
        "arceos",
        "axvm",
        "axvcpu",
        "axaddrspace",
        "arm_vcpu",
        "axdevice",
        "arm_vgic",
        "arm_gicv2",
        "axdevice_crates",
    ]

    for one in repos:
        p = f"{repo}/{one}.git"
        print(f"clone {p}")
        subprocess.run(
            ["git", "clone", p, f"crates/{one}"],
            check=True,
        )

    print("clone success")

    cargo_toml = ""

    with open("Cargo.toml", "r") as file:
        cargo_toml = file.read()

    os.rename("Cargo.toml", "Cargo.toml.bk")

    cargo_toml += """

[patch."https://github.com/arceos-hypervisor/arceos.git".axstd]
path = "crates/arceos/ulib/axstd"
[patch."https://github.com/arceos-hypervisor/arceos.git".axhal]
path = "crates/arceos/modules/axhal"
[patch."https://github.com/arceos-hypervisor/axvm.git".axvm]
path = "crates/axvm"
[patch."https://github.com/arceos-hypervisor/axvcpu.git".axvcpu]
path = "crates/axvcpu"
[patch."https://github.com/arceos-hypervisor/axaddrspace.git".axaddrspace]
path = "crates/axaddrspace"
[patch."https://github.com/arceos-hypervisor/arm_vcpu.git".arm_vcpu]
path = "crates/arm_vcpu"
[patch."https://github.com/arceos-hypervisor/axdevice.git".axdevice]
path = "crates/axdevice"
[patch."https://github.com/arceos-hypervisor/arm_vgic.git".arm_vgic]
path = "crates/arm_vgic"
[patch."https://github.com/arceos-hypervisor/axdevice_crates.git".axdevice_base]
path = "crates/axdevice_crates/axdevice_base"
[patch."https://github.com/arceos-hypervisor/arm_gicv2.git".arm_gicv2]
path = "crates/arm_gicv2"
"""

    with open("Cargo.toml", "w") as file:
        file.write(cargo_toml)

    # 创建 .vscode 目录并生成 settings.json
    os.makedirs(".vscode", exist_ok=True)
    with open(".vscode/settings.json", "w") as settings_json:
        settings_json.write(
            """
{
    "rust-analyzer.cargo.target": "aarch64-unknown-none-softfloat",
    "rust-analyzer.check.allTargets": false,
    "rust-analyzer.cargo.features": ["fs"],
    "rust-analyzer.cargo.extraEnv": {
        "AX_CONFIG_PATH": "${workspaceFolder}/.axconfig.toml"
    }
}
    """
        )

    print("patch success")


if __name__ == "__main__":
    main()
