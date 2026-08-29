# Negative declared-service nixosTest contract for the independent StarryOS test flake.
{ lib, pkgs, launcher }:
let
  boot = import ./boot.nix { inherit lib pkgs launcher; };
  starryMachine = ./starry_machine.py;
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
      wrap_machine = starry_machine.wrap_machine
      PHASE_GUEST_ASSERTION = starry_machine.PHASE_GUEST_ASSERTION

      import queue
      import re
      import time

      machine = wrap_machine(create_machine(${builtins.toJSON launcher}, name="starry-nixos-service-fail"))
      machine.start()

      terminal_deadline = time.monotonic() + 600
      terminal_seen = False
      terminal_pattern = ${builtins.toJSON boot.patterns.terminalPattern} + "|STARRY_NIXOS_ASSERT_FAILED:"
      while time.monotonic() < terminal_deadline:
          try:
              while True:
                  machine.last_lines.get(block=False)
          except queue.Empty:
              pass
          console = machine.get_console_log()
          if re.search(terminal_pattern, console):
              terminal_seen = True
              break
          if machine.process is not None and machine.process.poll() is not None:
              break
          time.sleep(1)

      console = machine.get_console_log()
      qemu_exited = machine.process is not None and machine.process.poll() is not None
      evaluate_boot_console(console, terminal_seen=terminal_seen, qemu_exited=qemu_exited)
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
