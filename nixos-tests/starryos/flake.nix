{
  description = "Independent StarryOS-backed nixosTest suite";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      mkAssembly =
        {
          kernelPath,
          kernelNarHash,
          starryNixos,
        }:
        import ./default.nix {
          lib = nixpkgs.lib;
          inherit pkgs kernelPath kernelNarHash;
          inherit (starryNixos)
            mkNixosSystem
            mkRootfs
            qemuConfig
            systemModule
            ;
        };
      mkStarryNixosTest =
        {
          kernelPath ? null,
          kernelNarHash ? null,
          starryNixos ? null,
        }:
        if starryNixos == null then
          throw "Starry nixosTest requires an explicit StarryNixOS app interface"
        else
          let
            assembly = mkAssembly { inherit kernelPath kernelNarHash starryNixos; };
          in
          assembly.test
          // {
            inherit (assembly) kernelStorePath systemToplevel;
          };
      fixtureSystem = pkgs.runCommand "starry-nixos-fixture-system" { } ''
        mkdir -p "$out"
        touch "$out/init"
      '';
      fixtureShared = {
        mkNixosSystem = _: {
          config.system.build.toplevel = fixtureSystem;
        };
        mkRootfs = _: pkgs.runCommand "starry-nixos-fixture-rootfs" { } "touch $out";
        systemModule = ./boot.nix;
        qemuConfig = ./boot.nix;
      };
      missingInputs = builtins.tryEval (
        (mkStarryNixosTest {
          kernelPath = null;
          kernelNarHash = null;
          starryNixos = fixtureShared;
        }).drvPath
      );
      fixture = mkAssembly {
        kernelPath = ./kernel-fixture.bin;
        kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        starryNixos = fixtureShared;
      };
      fixtureTest = mkStarryNixosTest {
        kernelPath = ./kernel-fixture.bin;
        kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        starryNixos = fixtureShared;
      };
      p1InterfaceCheck =
        assert missingInputs.success == false;
        assert fixture.kernelStorePath != ./kernel-fixture.bin;
        assert fixture.kernelNarHash == "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        assert fixture.systemToplevel == fixtureSystem;
        assert fixture.contract.nodes == { };
        assert fixture.contract.requiredFeatures.kvm == false;
        assert fixture.contract.requiredFeatures.devnet == false;
        assert fixture.contract.requiredFeatures.nixos-test == false;
        assert fixture.contract.enableOCR == false;
        assert fixture.contract.sshBackdoor.enable == false;
        assert fixture.contract.globalTimeout == 900;
        assert nixpkgs.lib.hasInfix ''-- "$@"'' fixture.launcherScript;
        assert fixtureTest.kernelStorePath == fixture.kernelStorePath;
        assert fixtureTest.systemToplevel == fixture.systemToplevel;
        assert (builtins.tryEval fixtureTest.drvPath).success;
        pkgs.runCommand "starry-nixos-p1-interface-check" {
          nativeBuildInputs = [ pkgs.python3 ];
          TERMINAL_PATTERN = fixture.terminalPattern;
        } ''
          python - <<'PY'
          import os
          import re

          re.compile(os.environ["TERMINAL_PATTERN"])
          PY
          touch "$out"
        '';
    in
    {
      lib.${system} = {
        inherit mkStarryNixosTest;
        requiresMatchingAppNixpkgs = true;
      };
      checks.${system}.p1-interface = p1InterfaceCheck;
    };
}
