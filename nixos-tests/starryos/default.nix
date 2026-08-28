{
  lib,
  pkgs,
  mkNixosSystem,
  mkRootfs,
  systemModule,
  qemuConfig,
  kernelPath ? null,
  kernelNarHash ? null,
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

  nixos = mkNixosSystem [ systemModule ];
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
  launcher = pkgs.writeShellScript "run-starry-nixos-boot-vm" launcherScript;
  boot = import ./boot.nix {
    inherit lib pkgs launcher;
  };
  contract = boot.contract;
in
{
  inherit checkedKernel contract launcherScript nixos toplevel rootfs;
  kernelStorePath = checkedKernel;
  kernelNarHash = kernelNarHash;
  systemToplevel = toplevel;
  terminalPattern = boot.patterns.terminalPattern;
  test = pkgs.testers.runNixOSTest contract;
}
