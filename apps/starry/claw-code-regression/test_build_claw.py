"""Host integration tests: real Git, Cargo and ext4, with a local upstream.

Run with python3 -m unittest discover -s apps/starry/claw-code-regression -p 'test_*.py'.
Only the upstream revision and compilation target in the script copy are adapted;
Git's URL rewrite supplies a disposable repository without a network dependency.
"""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


APPS = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/MuZhao2333/claw-code"


class PinnedBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="claw-build-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.upstream = self.root / "upstream"
        self.upstream.mkdir()
        self.env = os.environ.copy()
        self.env.update(
            GIT_CONFIG_GLOBAL=str(self.root / "gitconfig"),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_ALLOW_PROTOCOL="file",
            CARGO_NET_OFFLINE="true",
            PROBE_MARKER=str(self.root / "build-script-ran"),
        )
        self.env["RUSTUP_TOOLCHAIN"] = self.run_command(
            "rustup", "show", "active-toolchain", cwd=APPS
        ).stdout.split()[0]
        self.run_command("git", "init", "-q", str(self.upstream))
        self.run_command("git", "config", "--global", "user.email", "test@example.invalid")
        self.run_command("git", "config", "--global", "user.name", "Build test")
        self.run_command(
            "git", "config", "--global",
            f"url.{self.upstream.as_uri()}.insteadOf", REPOSITORY,
        )
        self.rust = self.upstream / "rust"
        (self.rust / "src").mkdir(parents=True)
        (self.rust / "Cargo.toml").write_text(
            '[package]\nname = "claw"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        (self.rust / "src/main.rs").write_text('fn main() { println!("pinned"); }\n')
        (self.rust / "build.rs").write_text(
            'fn main() { std::fs::write(std::env::var("PROBE_MARKER").unwrap(), '
            '"executed").unwrap(); }\n'
        )
        self.run_command("cargo", "generate-lockfile", "--offline", cwd=self.rust)
        self.pinned = self.commit()
        self.host = re.search(
            r"^host: (.+)$", self.run_command("rustc", "-vV").stdout, re.MULTILINE
        ).group(1)

    def run_command(self, *args, cwd=None, check=True, env=None):
        result = subprocess.run(
            args, cwd=cwd or self.root, env=env or self.env,
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        if check and result.returncode:
            self.fail(f"{args}: {result.returncode}\n{result.stdout}\n{result.stderr}")
        return result

    def commit(self):
        self.run_command("git", "add", ".", cwd=self.upstream)
        self.run_command("git", "commit", "-qm", "fixture", cwd=self.upstream)
        return self.run_command("git", "rev-parse", "HEAD", cwd=self.upstream).stdout.strip()

    def prepare(self, entry):
        workspace = self.root / entry
        for app, filename in (
            ("claw-code", "prebuild.sh"),
            ("claw-code-regression", "build-claw.sh"),
        ):
            destination = workspace / "apps/starry" / app / filename
            destination.parent.mkdir(parents=True, exist_ok=True)
            source = (APPS / app / filename).read_text()
            source = re.sub(r'CLAW_REV="[0-9a-f]{40}"', f'CLAW_REV="{self.pinned}"', source)
            destination.write_text(source.replace("x86_64-unknown-linux-musl", self.host))
        env = self.env | {
            "CLAW_CACHE_DIR": str(workspace / "cache"),
            "STARRY_WORKSPACE": str(workspace),
            "STARRY_BASE_ROOTFS": str(workspace / "base.img"),
            "STARRY_ROOTFS": str(workspace / "app.img"),
        }
        if entry == "prebuild":
            with (workspace / "base.img").open("wb") as image:
                image.truncate(32 * 1024 * 1024)
            self.run_command("mke2fs", "-q", "-t", "ext4", "-F", env["STARRY_BASE_ROOTFS"])
            for directory in ("/usr", "/usr/bin"):
                self.run_command("debugfs", "-w", "-R", f"mkdir {directory}", env["STARRY_BASE_ROOTFS"])
            script = workspace / "apps/starry/claw-code/prebuild.sh"
        else:
            script = workspace / "apps/starry/claw-code-regression/build-claw.sh"
        return workspace, env, script

    def binary(self, entry, workspace, result):
        if entry == "helper":
            return Path(result.stdout.strip().splitlines()[-1])
        binary = workspace / "installed-claw"
        binary.unlink(missing_ok=True)
        self.run_command(
            "debugfs", "-R", f"dump /usr/bin/claw {binary}", str(workspace / "app.img")
        )
        binary.chmod(0o755)
        return binary

    def test_pinned_build_and_verified_cache_ignore_moving_branch(self):
        (self.rust / "src/main.rs").write_text('fn main() { println!("moving-head"); }\n')
        self.commit()
        for entry in ("helper", "prebuild"):
            with self.subTest(entry=entry):
                workspace, env, script = self.prepare(entry)
                cache = Path(env["CLAW_CACHE_DIR"])
                cache.mkdir()
                result = self.run_command("bash", str(script), env=env)
                binary = self.binary(entry, workspace, result)
                self.assertEqual(self.run_command(str(binary)).stdout.strip(), "pinned")
                marker = Path(env["PROBE_MARKER"])
                marker.unlink()
                # A legacy cache must not override the verified, versioned build.
                (cache / "claw").write_text("unverified legacy binary")
                result = self.run_command("bash", str(script), env=env)
                self.assertFalse(marker.exists(), "valid cache should avoid rebuilding")
                cached = next(cache.rglob("claw.provenance"))
                provenance = cached.read_text()
                self.assertIn(self.pinned, provenance)
                self.assertIn("--locked", provenance)
                if entry == "helper":
                    self.assertEqual(result.stdout.strip(), str(binary))
                else:
                    installed = self.run_command(
                        "debugfs", "-R", "cat /usr/bin/claw.provenance",
                        str(workspace / "app.img"),
                    ).stdout
                    self.assertEqual(installed, provenance)
                cached.with_name("claw").write_text("unverified binary")
                result = self.run_command("bash", str(script), env=env)
                rebuilt = self.binary(entry, workspace, result)
                self.assertEqual(self.run_command(str(rebuilt)).stdout.strip(), "pinned")
                self.assertTrue(marker.exists(), "damaged binary must be rebuilt")
                marker.unlink()

    def test_cargo_consumer_rebuilds_embedded_binary_when_pin_changes(self):
        workspace, env, script = self.prepare("helper")
        consumer = script.parent / "integration/rust"
        (consumer / "src").mkdir(parents=True)
        (consumer / "Cargo.toml").write_text(
            '[workspace]\n[package]\nname = "consumer"\nversion = "0.1.0"\n'
        )
        (consumer / "build.rs").write_text(
            (APPS / "claw-code-regression/integration/rust/build.rs").read_text()
        )
        (consumer / "src/main.rs").write_text(
            'fn main() { std::fs::write(std::env::args().nth(1).unwrap(), '
            'include_bytes!(concat!(env!("OUT_DIR"), "/claw-binary"))).unwrap(); }'
        )
        embedded = workspace / "embedded-claw"

        def build_and_run():
            self.run_command("cargo", "build", "--offline", cwd=consumer, env=env)
            self.run_command(str(consumer / "target/debug/consumer"), str(embedded))
            embedded.chmod(0o755)
            return self.run_command(str(embedded)).stdout.strip()

        self.assertEqual(build_and_run(), "pinned")
        (self.rust / "src/main.rs").write_text('fn main() { println!("updated-pin"); }\n')
        updated = self.commit()
        script.write_text(script.read_text().replace(self.pinned, updated))
        # Ensure Cargo observes a strictly newer input, even on coarse filesystems.
        changed = script.stat().st_mtime_ns + 2_000_000_000
        os.utime(script, ns=(changed, changed))
        self.assertEqual(build_and_run(), "updated-pin")

    def test_lock_drift_fails_before_build_script_or_installation(self):
        dependency = self.rust / "dependency"
        (dependency / "src").mkdir(parents=True)
        (dependency / "Cargo.toml").write_text(
            '[package]\nname = "new-dependency"\nversion = "0.1.0"\n'
        )
        (dependency / "src/lib.rs").write_text("")
        with (self.rust / "Cargo.toml").open("a") as manifest:
            manifest.write('\n[dependencies]\nnew-dependency = { path = "dependency" }\n')
        self.pinned = self.commit()
        for entry in ("helper", "prebuild"):
            with self.subTest(entry=entry):
                workspace, env, script = self.prepare(entry)
                result = self.run_command("bash", str(script), env=env, check=False)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("--locked", result.stderr)
                self.assertFalse(Path(env["PROBE_MARKER"]).exists())
                self.assertFalse((workspace / "app.img").exists())
                self.assertFalse(list(Path(env["CLAW_CACHE_DIR"]).rglob("claw")))


if __name__ == "__main__":
    unittest.main()
