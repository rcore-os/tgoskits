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

  extraModules =
    if caseName == "boot" || caseName == "unsupported" then
      [ ]
    else if caseName == "service" then
      [
        ./modules/keep-running.nix
        ./modules/service-assert.nix
      ]
    else if caseName == "service-fail" then
      [
        ./modules/keep-running.nix
        ./modules/service-assert-fail.nix
      ]
    else
      throw "unsupported Starry nixosTest case `${caseName}`";

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
  selected =
    if caseName == "boot" then
      import ./boot.nix { inherit lib pkgs launcher; }
    else if caseName == "service" then
      import ./service.nix { inherit lib pkgs launcher; }
    else if caseName == "service-fail" then
      import ./service-fail.nix { inherit lib pkgs launcher; }
    else
      import ./unsupported.nix { inherit lib pkgs launcher; };
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
