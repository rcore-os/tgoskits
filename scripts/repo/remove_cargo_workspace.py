#!/usr/bin/env python3
"""
Cargo.toml workspace 配置管理工具。

用法:
    python3 remove_cargo_workspace.py <command> [options]

命令:
    remove <path>       移除 [workspace] 配置（默认命令）
    remove --all        移除 components/ 和 os/ 下所有子目录的 workspace 配置
    restore <path>      从 .bk 备份恢复原始文件

remove 功能:
    1. 读取指定的 Cargo.toml 文件
    2. 将原文件备份为 .bk 后缀
    3. 移除 [workspace] 及其所有子节点配置
    4. 如果移除后文件为空，则删除文件；否则保存到原文件名

remove --all 功能:
    1. 遍历 components/ 和 os/ 目录下所有子目录
    2. 查找每个子目录中的 Cargo.toml 文件
    3. 对每个文件执行 remove 操作（复用现有逻辑）
    4. 显示处理统计信息

restore 功能:
    1. 检查 .bk 备份文件是否存在
    2. 如果原文件不存在，直接用 .bk 改名恢复
    3. 否则，从 .bk 文件中提取所有 workspace 相关配置
    4. 将 workspace 配置添加到当前文件开头（保留当前文件的其他修改）
    5. 创建 .tmp 备份（安全措施）
    6. 删除 .bk 和 .tmp 临时文件
"""

import sys
import os
import shutil
from pathlib import Path


def extract_workspace_section(toml_lines):
    """
    从 TOML 内容中提取 [workspace] 及其子节点。
    返回包含 workspace 配置的行列表。
    """
    result = []
    i = 0

    while i < len(toml_lines):
        line = toml_lines[i]
        stripped = line.strip()

        # 检测 [workspace] 或 [workspace.xxx]
        if stripped.startswith('[workspace') and stripped.endswith(']'):
            # 添加整个 workspace 部分
            result.append(line)
            i += 1
            # 添加所有子节点，直到遇到同级或更高级别的表
            while i < len(toml_lines):
                next_line = toml_lines[i]
                next_stripped = next_line.strip()
                # 遇到新的表定义（非 workspace 子节点）
                if next_stripped.startswith('['):
                    if not next_stripped.startswith('[workspace'):
                        break
                    result.append(next_line)
                else:
                    result.append(next_line)
                i += 1
            break
        i += 1

    return result


def remove_workspace_section(toml_lines):
    """
    从 TOML 内容中移除 [workspace] 及其子节点。
    """
    result = []
    i = 0

    while i < len(toml_lines):
        line = toml_lines[i]
        stripped = line.strip()

        # 检测 [workspace] 或 [workspace.xxx]
        if stripped.startswith('[workspace') and stripped.endswith(']'):
            # 跳过整个 workspace 部分
            i += 1
            # 跳过所有子节点，直到遇到同级或更高级别的表
            while i < len(toml_lines):
                next_line = toml_lines[i]
                next_stripped = next_line.strip()
                # 遇到新的表定义（非 workspace 子节点）
                if next_stripped.startswith('['):
                    if not next_stripped.startswith('[workspace'):
                        break
                    i += 1
                else:
                    i += 1
            continue

        result.append(line)
        i += 1

    return result


def process_cargo_toml(file_path):
    """
    处理指定的 Cargo.toml 文件。
    """
    path = Path(file_path).resolve()

    if not path.exists():
        print(f"错误: 文件不存在: {path}", file=sys.stderr)
        return 1

    if not path.name == 'Cargo.toml':
        print(f"警告: 文件名不是 Cargo.toml: {path.name}", file=sys.stderr)

    # 读取文件内容
    try:
        with open(path, 'r', encoding='utf-8') as f:
            content = f.read()
    except Exception as e:
        print(f"错误: 无法读取文件: {e}", file=sys.stderr)
        return 1

    # 检查是否包含 workspace 配置
    if '[workspace' not in content:
        print(f"提示: 文件中未发现 [workspace] 配置，无需处理。")
        return 0

    # 备份原文件
    backup_path = path.with_name(f"{path.name}.bk")
    try:
        shutil.copy2(path, backup_path)
        print(f"已备份原文件到: {backup_path}")
    except Exception as e:
        print(f"错误: 无法创建备份文件: {e}", file=sys.stderr)
        return 1

    # 处理内容
    lines = content.splitlines(keepends=True)
    processed_lines = remove_workspace_section(lines)
    processed_content = ''.join(processed_lines)

    # 写入处理后的内容
    try:
        with open(path, 'w', encoding='utf-8') as f:
            f.write(processed_content)

        # 检查处理后内容是否为空（或仅包含空白字符）
        if not processed_content.strip():
            path.unlink()
            print(f"文件内容为空，已删除: {path}")
        else:
            print(f"已处理并保存: {path}")
        return 0
    except Exception as e:
        print(f"错误: 无法写入文件: {e}", file=sys.stderr)
        return 1


def process_all_cargo_tomls(base_dir=None):
    """
    遍历 components/ 和 os/ 目录下所有子目录的 Cargo.toml 文件，
    移除 workspace 配置。
    """
    if base_dir is None:
        base_dir = Path(__file__).parent.parent.parent  # 获取项目根目录

    # 定义要搜索的目录
    search_dirs = [
        base_dir / 'components',
        base_dir / 'os'
    ]

    stats = {'success': 0, 'skipped': 0, 'failed': 0, 'total': 0}

    for search_dir in search_dirs:
        if not search_dir.exists():
            print(f"警告: 目录不存在: {search_dir}", file=sys.stderr)
            continue

        # 遍历一层子目录
        for subdir in search_dir.iterdir():
            if not subdir.is_dir():
                continue

            cargo_toml = subdir / 'Cargo.toml'
            if not cargo_toml.exists():
                continue

            stats['total'] += 1
            print(f"\n处理 [{stats['total']}]: {cargo_toml}")

            result = process_cargo_toml(cargo_toml)
            if result == 0:
                # 检查是否被跳过（无 workspace 配置）
                backup_path = cargo_toml.with_name(f"{cargo_toml.name}.bk")
                if backup_path.exists():
                    stats['success'] += 1
                else:
                    stats['skipped'] += 1
            else:
                stats['failed'] += 1

    # 打印统计信息
    print(f"\n{'='*50}")
    print(f"处理完成！总计: {stats['total']} 个文件")
    print(f"  成功: {stats['success']}")
    print(f"  跳过: {stats['skipped']}")
    print(f"  失败: {stats['failed']}")
    print(f"{'='*50}")

    return 0 if stats['failed'] == 0 else 1


def restore_backup(file_path):
    """
    从 .bk 备份文件恢复 workspace 配置到当前文件。
    """
    path = Path(file_path).resolve()

    # 检查备份文件是否存在
    backup_path = path.with_name(f"{path.name}.bk")
    if not backup_path.exists():
        print(f"提示: 备份文件不存在，无需恢复: {backup_path}")
        return 0

    # 如果原文件不存在，直接用备份恢复
    if not path.exists():
        try:
            shutil.move(str(backup_path), str(path))
            print(f"原文件不存在，已从备份直接恢复: {path}")
            return 0
        except Exception as e:
            print(f"错误: 无法从备份恢复文件: {e}", file=sys.stderr)
            return 1

    # 创建临时备份（安全措施）
    tmp_path = path.with_name(f"{path.name}.tmp")
    try:
        shutil.copy2(path, tmp_path)
        print(f"已创建临时备份: {tmp_path}")
    except Exception as e:
        print(f"警告: 无法创建临时备份: {e}", file=sys.stderr)

    try:
        # 读取备份文件和当前文件
        with open(backup_path, 'r', encoding='utf-8') as f:
            backup_content = f.read()
        with open(path, 'r', encoding='utf-8') as f:
            current_content = f.read()

        # 从备份文件中提取 workspace 配置
        backup_lines = backup_content.splitlines(keepends=True)
        workspace_lines = extract_workspace_section(backup_lines)

        if not workspace_lines:
            print(f"警告: 备份文件中未找到 workspace 配置")
            # 仍然继续，只是不添加任何内容

        # 检查当前文件是否已包含 workspace 配置
        if '[workspace' in current_content:
            print(f"警告: 当前文件已包含 workspace 配置，将被覆盖")

        # 移除当前文件中可能存在的 workspace 配置
        current_lines = current_content.splitlines(keepends=True)
        current_lines = remove_workspace_section(current_lines)
        current_content = ''.join(current_lines)

        # 合并：workspace 配置 + 当前文件内容
        workspace_content = ''.join(workspace_lines)
        merged_content = workspace_content + current_content

        # 写入合并后的内容
        with open(path, 'w', encoding='utf-8') as f:
            f.write(merged_content)
        print(f"已恢复 workspace 配置到: {path}")

        # 删除 .bk 和 .tmp 文件
        files_to_delete = [backup_path]
        if tmp_path.exists():
            files_to_delete.append(tmp_path)

        for f in files_to_delete:
            try:
                f.unlink()
                print(f"已删除临时文件: {f}")
            except Exception as e:
                print(f"警告: 无法删除临时文件 {f}: {e}", file=sys.stderr)

        return 0
    except Exception as e:
        print(f"错误: 无法恢复 workspace 配置: {e}", file=sys.stderr)
        # 如果恢复失败，尝试恢复临时备份
        if tmp_path.exists():
            try:
                shutil.copy2(tmp_path, path)
                print(f"已从临时备份恢复: {tmp_path}")
            except:
                pass
        return 1


def print_usage():
    """打印使用说明"""
    print(__doc__)


def main():
    if len(sys.argv) < 2:
        print_usage()
        print("\n错误: 请指定命令和文件路径", file=sys.stderr)
        sys.exit(1)

    command = sys.argv[1]

    # 支持直接指定文件路径（默认为 remove 命令）
    if command in ['remove', '--remove', '-r']:
        if len(sys.argv) < 3:
            print_usage()
            print("\n错误: 请指定 Cargo.toml 文件路径或 --all 选项", file=sys.stderr)
            sys.exit(1)

        # 检查是否为 --all 选项
        if sys.argv[2] in ['--all', '-a']:
            sys.exit(process_all_cargo_tomls())
        else:
            file_path = sys.argv[2]
            sys.exit(process_cargo_toml(file_path))
    elif command in ['restore', '--restore', '-rs']:
        if len(sys.argv) < 3:
            print_usage()
            print("\n错误: 请指定 Cargo.toml 文件路径", file=sys.stderr)
            sys.exit(1)
        file_path = sys.argv[2]
        sys.exit(restore_backup(file_path))
    elif command in ['--help', '-h', 'help']:
        print_usage()
        sys.exit(0)
    else:
        # 默认当作 remove 命令处理
        file_path = command
        sys.exit(process_cargo_toml(file_path))


if __name__ == '__main__':
    main()
