# Upstream nixosTest lifecycle contract for the independent StarryOS test flake.
{ lib, pkgs, launcher, starryMachine }:
let
  successPattern = "(?s:"
    + builtins.concatStringsSep ".*" [
    "STARRY_NIXOS_PHASE=pid1"
    "STARRY_NIXOS_PHASE=activation"
    "STARRY_NIXOS_PHASE=systemd"
    "STARRY_NIXOS_PHASE=marker"
    "STARRY_NIXOS_SYSTEM_PASSED"
  ]
    + ")";
  failurePatterns = [
    "(?i:\\bpanic(?:ked)?\\b)"
    "(?i:\\bfatal\\b)"
    "(?m:^.*(?:starry-nixos-)?marker\\.service: Failed with result)"
    "(?m:^Failed to start Verify the StarryNixOS stage-2 baseline\\.?$)"
    "(?m:^STARRY_NIXOS_SYSTEM_FAILED:)"
  ];
  # The upstream driver waits for one terminal line at a time. The full ordered
  # success expression is checked against the complete console after wakeup.
  terminalPattern = "STARRY_NIXOS_SYSTEM_PASSED|" + builtins.concatStringsSep "|" failurePatterns;
in
{
  patterns = {
    inherit failurePatterns successPattern terminalPattern;
  };
  contract = {
    name = "starry-nixos-boot";
    nodes = { };
    requiredFeatures = {
      kvm = false;
      devnet = false;
      nixos-test = false;
    };
    enableOCR = false;
    sshBackdoor.enable = false;
    globalTimeout = 900;
    testScript = ''
      import importlib.util
      spec = importlib.util.spec_from_file_location("starry_machine", ${builtins.toJSON (toString starryMachine)})
      assert spec is not None and spec.loader is not None
      starry_machine = importlib.util.module_from_spec(spec)
      spec.loader.exec_module(starry_machine)
      evaluate_boot_console = starry_machine.evaluate_boot_console
      raise_phase = starry_machine.raise_phase
      wait_for_console_evidence = starry_machine.wait_for_console_evidence
      PHASE_SHUTDOWN = starry_machine.PHASE_SHUTDOWN

      import time

      machine = create_machine(${builtins.toJSON launcher}, name="starry-nixos-boot")
      machine.start()

      console, terminal_seen, qemu_exited = wait_for_console_evidence(
          machine,
          ${builtins.toJSON terminalPattern},
          deadline=time.monotonic() + 600,
      )
      evaluate_boot_console(console, terminal_seen=terminal_seen, qemu_exited=qemu_exited)

      machine.wait_for_shutdown()
      if machine.process is None or machine.process.returncode != 0:
          raise_phase(PHASE_SHUTDOWN, "StarryNixOS QEMU did not exit successfully", console)
    '';
  };
}
