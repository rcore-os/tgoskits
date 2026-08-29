# Negative declared-service nixosTest contract for the independent StarryOS test flake.
{ lib, pkgs, launcher, starryMachine }:
let
  boot = import ./boot.nix { inherit lib pkgs launcher starryMachine; };
in
{
  inherit (boot) patterns;
  contract = boot.contract // {
    name = "starry-nixos-service-fail";
    testScript = ''
      import importlib.util
      spec = importlib.util.spec_from_file_location("starry_machine", ${builtins.toJSON (toString starryMachine)})
      assert spec is not None and spec.loader is not None
      starry_machine = importlib.util.module_from_spec(spec)
      spec.loader.exec_module(starry_machine)
      evaluate_boot_console = starry_machine.evaluate_boot_console
      evaluate_service_assertion = starry_machine.evaluate_service_assertion
      raise_phase = starry_machine.raise_phase
      wait_for_console_evidence = starry_machine.wait_for_console_evidence
      wrap_machine = starry_machine.wrap_machine
      PHASE_GUEST_ASSERTION = starry_machine.PHASE_GUEST_ASSERTION
      PHASE_TIMEOUT = starry_machine.PHASE_TIMEOUT

      import time

      machine = wrap_machine(create_machine(${builtins.toJSON launcher}, name="starry-nixos-service-fail"))
      machine.start()

      global_deadline = time.monotonic() + 900
      console, terminal_seen, qemu_exited = wait_for_console_evidence(
          machine,
          ${builtins.toJSON boot.patterns.terminalPattern},
          deadline=min(time.monotonic() + 600, global_deadline),
      )
      evaluate_boot_console(console, terminal_seen=terminal_seen, qemu_exited=qemu_exited)

      console, assertion_seen, qemu_exited = wait_for_console_evidence(
          machine,
          r"STARRY_NIXOS_ASSERT_PASSED|STARRY_NIXOS_ASSERT_FAILED:",
          deadline=global_deadline,
      )
      if not assertion_seen:
          raise_phase(
              PHASE_TIMEOUT,
              "StarryNixOS service-fail produced no assertion record before the global deadline",
              console,
          )
      record = evaluate_service_assertion(console, require_pass=False)
      machine.wait_for_shutdown()
      raise_phase(
          PHASE_GUEST_ASSERTION,
          "failed expectation: " + (record.get("reason") or "ASSERT_FAILED"),
          console,
      )
    '';
  };
}
