{
  launcher,
  starryMachine,
  caseName,
  kind,
  expectedStatus ? 0,
  expectedOutput ? null,
  expectPass ? true,
}:
let
  successPattern =
    "(?s:"
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
  terminalPattern = "STARRY_NIXOS_SYSTEM_PASSED|" + builtins.concatStringsSep "|" failurePatterns;
  starryMachinePath = builtins.toJSON (toString starryMachine);
  launcherJson = builtins.toJSON launcher;
  machineName = "starry-nixos-${caseName}";
  loadEvaluator = ''
    import importlib.util
    spec = importlib.util.spec_from_file_location("starry_machine", ${starryMachinePath})
    assert spec is not None and spec.loader is not None
    starry_machine = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(starry_machine)
    evaluate_boot_console = starry_machine.evaluate_boot_console
    evaluate_service_assertion = starry_machine.evaluate_service_assertion
    raise_phase = starry_machine.raise_phase
    wait_for_console_evidence = starry_machine.wait_for_console_evidence
    wrap_machine = starry_machine.wrap_machine
    PHASE_GUEST_ASSERTION = starry_machine.PHASE_GUEST_ASSERTION
    PHASE_SHUTDOWN = starry_machine.PHASE_SHUTDOWN
    PHASE_TIMEOUT = starry_machine.PHASE_TIMEOUT
  '';
  bootScript = ''
    ${loadEvaluator}
    import time

    machine = wrap_machine(create_machine(${launcherJson}, name=${builtins.toJSON machineName}))
    machine.start()

    console, terminal_seen, qemu_exited = wait_for_console_evidence(
        machine,
        ${builtins.toJSON terminalPattern},
        deadline=time.monotonic() + 300,
    )
    evaluate_boot_console(console, terminal_seen=terminal_seen, qemu_exited=qemu_exited)

    machine.wait_for_shutdown()
    if machine.process is None or machine.process.returncode != 0:
        raise_phase(PHASE_SHUTDOWN, "StarryNixOS QEMU did not exit successfully", console)
  '';
  expectedOutputPy = if expectedOutput == null then "None" else builtins.toJSON expectedOutput;
  requirePassPy = if expectPass then "True" else "False";
  assertScript = ''
    ${loadEvaluator}
    import time

    machine = wrap_machine(create_machine(${launcherJson}, name=${builtins.toJSON machineName}))
    machine.start()

    global_deadline = time.monotonic() + 300
    console, terminal_seen, qemu_exited = wait_for_console_evidence(
        machine,
        ${builtins.toJSON terminalPattern},
        deadline=min(time.monotonic() + 300, global_deadline),
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
            "StarryNixOS ${caseName} produced no assertion record before the global deadline",
            console,
        )
    record = evaluate_service_assertion(
        console,
        expected_status=${toString expectedStatus},
        expected_output=${expectedOutputPy},
        require_pass=${requirePassPy},
    )
    machine.wait_for_shutdown()
    ${
      if expectPass then
        ''
          if machine.process is None or machine.process.returncode != 0:
              raise_phase(PHASE_SHUTDOWN, "StarryNixOS QEMU did not exit successfully", console)
        ''
      else
        ''
          raise_phase(
              PHASE_GUEST_ASSERTION,
              "failed expectation: " + (record.get("reason") or "ASSERT_FAILED"),
              console,
          )
        ''
    }
  '';
  unsupportedScript = ''
    ${loadEvaluator}

    machine = wrap_machine(create_machine(${launcherJson}, name=${builtins.toJSON machineName}))
    machine.succeed("true")
  '';
  testScript =
    if kind == "boot" then
      bootScript
    else if kind == "assert" then
      assertScript
    else
      unsupportedScript;
in
{
  patterns = {
    inherit failurePatterns successPattern terminalPattern;
  };
  contract = {
    name = machineName;
    nodes = { };
    requiredFeatures = {
      kvm = false;
      devnet = false;
      nixos-test = false;
    };
    enableOCR = false;
    globalTimeout = 300;
    inherit testScript;
  };
}
