#!/usr/bin/env python3

import tomllib
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
GUEST_RESTART_WORKER = REPOSITORY_ROOT / "os/axvisor/src/guest_restart.rs"
AXVISOR_MANAGER = REPOSITORY_ROOT / "os/axvisor/src/manager.rs"
AXVM_VM = REPOSITORY_ROOT / "virtualization/axvm/src/vm/mod.rs"
AXVM_RESET_MEMORY = REPOSITORY_ROOT / "virtualization/axvm/src/vm/reset_memory.rs"
AXVM_ARCH_OPS = REPOSITORY_ROOT / "virtualization/axvm/src/architecture/ops.rs"
AXVM_AARCH64 = REPOSITORY_ROOT / "virtualization/axvm/src/arch/aarch64/mod.rs"
AXVM_RUNTIME = REPOSITORY_ROOT / "virtualization/axvm/src/runtime/mod.rs"
AXVM_VCPU_RUNTIME = REPOSITORY_ROOT / "virtualization/axvm/src/runtime/vcpus.rs"
GUEST_RESTART_BUILD = (
    REPOSITORY_ROOT
    / "competition/ivc/config/axvisor-orangepi-5-plus-restart.toml"
)


class AxvisorGuestRestartContractTests(unittest.TestCase):
    def test_restart_restores_the_pristine_guest_memory_image(self) -> None:
        worker_source = GUEST_RESTART_WORKER.read_text(encoding="utf-8")
        vm_source = AXVM_VM.read_text(encoding="utf-8")

        self.assertIn("capture_reset_memory", worker_source)
        self.assertIn("restore_reset_memory", vm_source)

    def test_restart_quiesces_guest_cpu_caches_before_restoring_ram(self) -> None:
        reset_source = AXVM_RESET_MEMORY.read_text(encoding="utf-8")
        arch_ops_source = AXVM_ARCH_OPS.read_text(encoding="utf-8")
        aarch64_source = AXVM_AARCH64.read_text(encoding="utf-8")
        runtime_source = AXVM_VCPU_RUNTIME.read_text(encoding="utf-8")

        task_exit = aarch64_source.split("fn before_vcpu_task_exit", maxsplit=1)[1]
        task_exit = task_exit.split("fn handle_vcpu_exit_bound", maxsplit=1)[0]
        self.assertIn("quiesce_local_reset_memory_cache", task_exit)
        self.assertIn("fn clean_and_invalidate_dcache_range", arch_ops_source)
        self.assertIn("CacheOp::CleanAndInvalidate", aarch64_source)
        self.assertNotIn("run_on_cpu_sync", aarch64_source)
        self.assertIn("count_ones() == 1", reset_source)

        stopping = runtime_source.split("if vm.stopping()", maxsplit=1)[1]
        stopping = stopping.split("break;", maxsplit=1)[0]
        self.assertLess(
            stopping.index("before_vcpu_task_exit"),
            stopping.index("mark_vcpu_exiting"),
        )

    def test_post_start_worker_stays_on_the_reserved_host_cpu(self) -> None:
        source = GUEST_RESTART_WORKER.read_text(encoding="utf-8")
        with GUEST_RESTART_BUILD.open("rb") as config_file:
            restart = tomllib.load(config_file)["guest_restart"]

        self.assertEqual(restart["cpu"], 3)
        self.assertIn("api::task::ax_set_current_affinity", source)
        self.assertIn("core::hint::spin_loop();", source)
        self.assertNotIn("fn wait_cooperatively", source)
        self.assertNotIn("thread::sleep(", source)

    def test_reserved_worker_does_not_yield_inside_vm_stop_wait(self) -> None:
        worker_source = GUEST_RESTART_WORKER.read_text(encoding="utf-8")
        manager_source = AXVISOR_MANAGER.read_text(encoding="utf-8")
        runtime_source = AXVM_RUNTIME.read_text(encoding="utf-8")
        vm_source = AXVM_VM.read_text(encoding="utf-8")

        self.assertIn("reset_vm_with_spin_wait", worker_source)
        self.assertIn("reset_vm_with_spin_wait", manager_source)
        self.assertIn("reset_vm_with_wait", runtime_source)
        self.assertIn("reset_with_wait", vm_source)


if __name__ == "__main__":
    unittest.main()
