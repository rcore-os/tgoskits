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
      if not terminal_seen:
          raise Exception("StarryNixOS boot produced no terminal evidence within 600 seconds")

      if not re.search(${builtins.toJSON successPattern}, console):
          raise Exception("StarryNixOS boot did not reach the ordered success contract")

      for failure_pattern in ${builtins.toJSON failurePatterns}:
          if re.search(failure_pattern, console):
              raise Exception(
                  "StarryNixOS boot matched a terminal failure pattern: "
                  + failure_pattern
              )

      machine.wait_for_shutdown()
      if machine.process is None or machine.process.returncode != 0:
          raise Exception("StarryNixOS QEMU did not exit successfully")
    '';
  };
}
