{
  lib,
  pkgs,
  mkNixosSystem,
  mkRootfs,
  systemModule,
  qemuConfig,
  kernelPath ? null,
  kernelNarHash ? null,
  caseName ? "boot",
}:
let
  inherit (builtins) path;

  checkedKernel =
    if kernelPath == null || kernelNarHash == null then
      throw "Starry nixosTest requires both kernelPath and kernelNarHash"
    else
      path {
        name = "starry-nixos-kernel";
        path = kernelPath;
        sha256 = kernelNarHash;
      };

  casePath = ./cases + "/${caseName}.nix";
  caseRecord =
    if builtins.pathExists casePath then
      import casePath
    else
      throw "unsupported Starry nixosTest case `${caseName}`";

  kind = caseRecord.kind or (throw "Starry nixosTest case `${caseName}` is missing kind");
  extraFromRecord = caseRecord.extraModules or [ ];
  command = caseRecord.command or null;
  expectedStatus = caseRecord.expectedStatus or 0;
  expectedOutput = caseRecord.expectedOutput or null;
  expectPass' = caseRecord.expectPass or (kind != "unsupported");
  packagesFn = caseRecord.packages or (_: [ ]);
  packages = if builtins.isFunction packagesFn then packagesFn pkgs else packagesFn;

  validatedKind =
    if kind == "boot" || kind == "assert" || kind == "unsupported" then
      kind
    else
      throw "Starry nixosTest case `${caseName}` has unknown kind `${kind}`";

  validatedCommand =
    if validatedKind == "assert" then
      if command == null then
        throw "Starry nixosTest assert case `${caseName}` requires command"
      else
        command
    else if command != null then
      throw "Starry nixosTest case `${caseName}` of kind `${validatedKind}` must not set command"
    else
      command;

  generatedAssert =
    if validatedKind == "assert" then
      [
        ./modules/keep-running.nix
        (
          import ./lib/declared-assert.nix {
            inherit pkgs lib;
            command = validatedCommand;
            expectPass = expectPass';
            inherit packages;
          }
        )
      ]
    else
      [ ];

  extraModules = import ./lib/baseline-guard.nix {
    inherit lib;
    extraModules = generatedAssert ++ extraFromRecord;
  };

  nixos = mkNixosSystem ([ systemModule ] ++ extraModules);
  toplevel = nixos.config.system.build.toplevel;
  rootfs = mkRootfs toplevel;
  qemu = lib.getExe' pkgs.qemu_test "qemu-system-x86_64";
  qemuImg = lib.getExe' pkgs.qemu-utils "qemu-img";
  ovmfCode = "${pkgs.OVMF.fd}/FV/OVMF_CODE.fd";
  ovmfVars = "${pkgs.OVMF.variables}";
  launcherScript = ''
    exec ${pkgs.python3}/bin/python ${./launch-vm.py} \
      --qemu-config ${qemuConfig} \
      --qemu ${qemu} \
      --qemu-img ${qemuImg} \
      --ovmf-code ${ovmfCode} \
      --ovmf-vars ${ovmfVars} \
      --kernel ${checkedKernel} \
      --rootfs ${rootfs} \
      --run-dir "$TMPDIR" \
      -- "$@"
  '';
  launcher = pkgs.writeShellScript "run-starry-nixos-${caseName}-vm" launcherScript;
  starryMachine = pkgs.writeText "starry_machine.py" (builtins.readFile ./starry_machine.py);
  selected = import ./lib/mkCase.nix {
    inherit
      launcher
      starryMachine
      caseName
      ;
    kind = validatedKind;
    inherit expectedStatus expectedOutput;
    expectPass = expectPass';
  };
  contract = selected.contract;
in
{
  inherit checkedKernel contract extraModules launcherScript nixos toplevel rootfs;
  kernelStorePath = checkedKernel;
  kernelNarHash = kernelNarHash;
  systemToplevel = toplevel;
  inherit caseName;
  terminalPattern = selected.patterns.terminalPattern;
  test = pkgs.testers.runNixOSTest contract;
}
