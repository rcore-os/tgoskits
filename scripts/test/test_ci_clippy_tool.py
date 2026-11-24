"""Verify host-tool restoration and workflow reuse without invoking a compiler."""
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import textwrap
import unittest

from check_ci_routing import mapping_block, named_step_block
from ci_clippy_tool import pack, restore_archive, unpack

ROOT = Path(__file__).resolve().parents[2]


def shell_step(name):
    workflow = (ROOT / '.github/workflows/clippy-checks.yml').read_text()
    plan = mapping_block(mapping_block(workflow, 'jobs', 0), 'plan', 2)
    block = named_step_block(plan, name)
    lines = block.splitlines()
    index = next(i for i, line in enumerate(lines) if line.strip() == 'run: |')
    return textwrap.dedent('\n'.join(lines[index + 1:]))


class ToolReuseTests(unittest.TestCase):
    def test_bootstrap_restores_or_explicitly_builds_and_propagates_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'scripts').symlink_to(ROOT / 'scripts', target_is_directory=True)
            (root / 'xtask-bin').mkdir()
            archive = root / 'xtask-bin/tg-xtask-bin.tar'
            payload = b'#!/bin/sh\nexit 0\n'
            with tarfile.open(archive, 'w') as tar:
                member = tarfile.TarInfo('target/debug/tg-xtask')
                member.size = len(payload)
                tar.addfile(member, io.BytesIO(payload))
            env = dict(os.environ, RUNNER_TEMP=str(root))
            restore = shell_step('Restore the preflight tool')
            result = subprocess.run(['bash', '-e', '-c', restore], cwd=root, env=env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / 'target/debug/tg-xtask').read_bytes(), payload)
            archive.unlink()
            # A stale local binary cannot turn a missing download into success.
            result = subprocess.run(['bash', '-e', '-c', restore], cwd=root, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            (root / 'shim').mkdir()
            cargo = root / 'shim/cargo'
            cargo.write_text('#!/bin/sh\n[ "$*" = "build --locked -p tg-xtask" ] || exit 97\nexit "$BUILD_STATUS"\n')
            cargo.chmod(0o755)
            env['PATH'] = str(root / 'shim') + os.pathsep + env['PATH']
            for status in ['0', '29']:
                result = subprocess.run(['bash', '-e', '-c', shell_step('Build the tool for an independent call')],
                                        cwd=root, env=dict(env, BUILD_STATUS=status), capture_output=True)
                self.assertEqual(result.returncode, int(status), result.stderr)
        workflow = (ROOT / '.github/workflows/clippy-checks.yml').read_text()
        for name in ['Validate preflight tool revision', 'Download the preflight tool', 'Restore the preflight tool']:
            self.assertIn('if: inputs.reuse_preflight_tool', named_step_block(workflow, name))
        self.assertIn('if: ${{ !inputs.reuse_preflight_tool }}', named_step_block(workflow, 'Build the tool for an independent call'))
        caller = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertIn("reuse_preflight_tool: ${{ needs.static_checks.result == 'success' }}", caller)

    def test_preflight_archive_is_atomic_and_rejects_unsafe_payloads(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'tool.tar'
            destination = root / 'bin/tool'
            destination.parent.mkdir()
            destination.write_bytes(b'previous')
            for name, kind, payload, extra in [
                ('target/debug/tg-xtask', tarfile.REGTYPE, b'payload', False),
                ('../outside', tarfile.REGTYPE, b'bad', False),
                ('target/debug/tg-xtask', tarfile.SYMTYPE, b'', False),
                ('target/debug/tg-xtask', tarfile.LNKTYPE, b'', False),
                ('target/debug/tg-xtask', tarfile.REGTYPE, b'', False),
                ('target/debug/tg-xtask', tarfile.REGTYPE, b'bad', True),
            ]:
                with self.subTest(name=name, kind=kind, extra=extra):
                    destination.write_bytes(b'previous')
                    with tarfile.open(archive, 'w') as tar:
                        member = tarfile.TarInfo(name)
                        member.type, member.size, member.linkname = kind, len(payload), '../outside'
                        tar.addfile(member, io.BytesIO(payload))
                        if extra:
                            tar.addfile(tarfile.TarInfo('extra'))
                    if payload == b'payload':
                        restore_archive(archive, 'r:', 'target/debug/tg-xtask', destination)
                        self.assertEqual(destination.read_bytes(), payload)
                        self.assertTrue(os.access(destination, os.X_OK))
                    else:
                        with self.assertRaises(ValueError):
                            restore_archive(archive, 'r:', 'target/debug/tg-xtask', destination)
                        self.assertEqual(destination.read_bytes(), b'previous')
            archive.write_bytes(b'corrupt')
            with self.assertRaises(tarfile.ReadError):
                restore_archive(archive, 'r:', 'target/debug/tg-xtask', destination)
            self.assertEqual(destination.read_bytes(), b'previous')

    def test_shard_archive_rejects_corruption_after_refactored_restoration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # A real executable exercises strip and the compressed archive path.
            pack(Path('/bin/true'), root / 'archive')
            unpack(root / 'archive', root / 'tool')
            subprocess.run([str(root / 'tool')], check=True)
            payload = root / 'archive/clippy-tool.tar.xz'
            payload.write_bytes(payload.read_bytes() + b'corrupt')
            before = (root / 'tool').read_bytes()
            with self.assertRaises(ValueError):
                unpack(root / 'archive', root / 'tool')
            self.assertEqual(before, (root / 'tool').read_bytes())

    def test_reused_selection_never_invokes_cargo_and_rejects_wrong_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            subprocess.run(['git', '-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid',
                            'commit', '--allow-empty', '-qm', 'base'], cwd=root, check=True)
            sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
            (root / 'scripts').symlink_to(ROOT / 'scripts', target_is_directory=True)
            (root / 'target/debug').mkdir(parents=True)
            binary = root / 'target/debug/tg-xtask'
            document = {'schema_version': 1, 'mode': 'full', 'tested_sha': sha,
                        'diff_base_sha': sha, 'selection_digest': 'c' * 64, 'checks': ['fixture']}
            binary.write_text('#!/bin/sh\nprintf \'%s\\n\' \'' + json.dumps(document) + '\'\n')
            binary.chmod(0o755)
            (root / 'shim').mkdir()
            cargo = root / 'shim/cargo'
            cargo.write_text('#!/bin/sh\necho UNEXPECTED_CARGO_BUILD >&2\nexit 97\n')
            cargo.chmod(0o755)
            output = root / 'output'
            env = dict(os.environ, PATH=str(root / 'shim') + os.pathsep + os.environ['PATH'],
                       EXPECTED_REVISION=sha, SINCE_REF='', FORCE_FULL='true',
                       INCREMENTAL_TIMEOUT='120', GITHUB_OUTPUT=str(output))
            result = subprocess.run(['bash', '-e', '-c', shell_step('Validate timeout and select checks')],
                                    cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('mode=full', output.read_text())
            for expected, success in [(sha, True), ('f' * 40, False)]:
                result = subprocess.run(['bash', '-e', '-c', shell_step('Validate preflight tool revision')],
                                        cwd=root, env=dict(env, EXPECTED_REVISION=expected), capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, success, result.stderr)
