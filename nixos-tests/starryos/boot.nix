# Upstream nixosTest lifecycle contract for the independent StarryOS test flake.
{ lib, pkgs, launcher }:
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
  starryMachine = ./starry_machine.py;
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
      PHASE_SHUTDOWN = starry_machine.PHASE_SHUTDOWN

      import queue
      import re
      import time

      machine = create_machine(${builtins.toJSON launcher}, name="starry-nixos-boot")
      machine.start()

      terminal_deadline = time.monotonic() + 600
      terminal_seen = False
      while time.monotonic() < terminal_deadline:
          # The driver stores every line in full_console_log but its public
          # wait helper consumes only one queued line per retry. Drain queued
          # lines first; otherwise normal Starry kernel diagnostics can delay
          # the terminal marker beyond the 600-second budget.
          try:
              while True:
                  machine.last_lines.get(block=False)
          except queue.Empty:
              pass
          console = machine.get_console_log()
          if re.search(${builtins.toJSON terminalPattern}, console):
              terminal_seen = True
              break
          if machine.process is not None and machine.process.poll() is not None:
              break
          time.sleep(1)

      console = machine.get_console_log()
      qemu_exited = machine.process is not None and machine.process.poll() is not None
      evaluate_boot_console(console, terminal_seen=terminal_seen, qemu_exited=qemu_exited)

      machine.wait_for_shutdown()
      if machine.process is None or machine.process.returncode != 0:
          raise_phase(PHASE_SHUTDOWN, "StarryNixOS QEMU did not exit successfully", console)
    '';
  };
}
