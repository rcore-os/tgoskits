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
          caseName ? "boot",
        }:
        import ./default.nix {
          lib = nixpkgs.lib;
          inherit pkgs kernelPath kernelNarHash caseName;
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
          caseName ? "boot",
        }:
        if starryNixos == null then
          throw "Starry nixosTest requires an explicit StarryNixOS app interface"
        else
          let
            assembly = mkAssembly {
              inherit kernelPath kernelNarHash starryNixos caseName;
            };
          in
          assembly.test
          // {
            inherit (assembly) kernelStorePath systemToplevel extraModules caseName;
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
        systemModule = ./kernel-fixture.bin;
        qemuConfig = ./kernel-fixture.bin;
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
      fixtureService = mkAssembly {
        kernelPath = ./kernel-fixture.bin;
        kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        starryNixos = fixtureShared;
        caseName = "service";
      };
      fixtureServiceFail = mkAssembly {
        kernelPath = ./kernel-fixture.bin;
        kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        starryNixos = fixtureShared;
        caseName = "service-fail";
      };
      fixtureUnsupported = mkAssembly {
        kernelPath = ./kernel-fixture.bin;
        kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        starryNixos = fixtureShared;
        caseName = "unsupported";
      };
      fixtureHello = mkAssembly {
        kernelPath = ./kernel-fixture.bin;
        kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
        starryNixos = fixtureShared;
        caseName = "hello-tmpfiles";
      };
      unknownCase = builtins.tryEval (
        (mkAssembly {
          kernelPath = ./kernel-fixture.bin;
          kernelNarHash = "sha256-fiqck+CYjhisfQB5RvFEY3jAVmNFkaT5oXtmB/O54kk=";
          starryNixos = fixtureShared;
          caseName = "does-not-exist";
        }).extraModules
      );
      udevGuard = builtins.tryEval (
        (import ./lib/baseline-guard.nix {
          lib = nixpkgs.lib;
          extraModules = [ { services.udev.enable = true; } ];
        })
      );
      extraModuleHasKeepRunning =
        extraModules:
        nixpkgs.lib.any (
          module: module == ./modules/keep-running.nix
        ) extraModules;
      extraModuleHasHelloTmpfiles =
        extraModules:
        nixpkgs.lib.any (
          module: module == ./modules/hello-tmpfiles.nix
        ) extraModules;
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
        assert fixture.extraModules == [ ];
        assert fixture.caseName == "boot";
        assert nixpkgs.lib.hasInfix ''-- "$@"'' fixture.launcherScript;
        assert fixtureTest.kernelStorePath == fixture.kernelStorePath;
        assert fixtureTest.systemToplevel == fixture.systemToplevel;
        assert fixtureTest.extraModules == [ ];
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

      p4InterfaceCheck =
        assert unknownCase.success == false;
        assert fixture.extraModules == [ ];
        assert extraModuleHasKeepRunning fixtureService.extraModules;
        assert extraModuleHasKeepRunning fixtureServiceFail.extraModules;
        assert extraModuleHasKeepRunning fixtureHello.extraModules;
        assert extraModuleHasHelloTmpfiles fixtureHello.extraModules;
        assert fixtureUnsupported.extraModules == [ ];
        assert fixtureService.contract.name == "starry-nixos-service";
        assert fixtureServiceFail.contract.name == "starry-nixos-service-fail";
        assert fixtureUnsupported.contract.name == "starry-nixos-unsupported";
        assert fixtureHello.contract.name == "starry-nixos-hello-tmpfiles";
        assert nixpkgs.lib.hasInfix "STARRY_NIXOS_ASSERT_PASSED" fixtureService.contract.testScript;
        assert nixpkgs.lib.hasInfix "STARRY_NIXOS_ASSERT_FAILED" fixtureServiceFail.contract.testScript;
        assert nixpkgs.lib.hasInfix "succeed(\"true\")" fixtureUnsupported.contract.testScript;
        assert nixpkgs.lib.hasInfix "STARRY_NIXOS_ASSERT_PASSED" fixtureHello.contract.testScript;
        assert nixpkgs.lib.hasInfix "wrap_machine" fixture.contract.testScript;
        assert udevGuard.success == false;
        assert (builtins.tryEval fixtureService.test.drvPath).success;
        assert (builtins.tryEval fixtureHello.test.drvPath).success;
        pkgs.runCommand "starry-nixos-p4-interface-check" { } ''
          touch "$out"
        '';
    in
    {
      lib.${system} = {
        inherit mkStarryNixosTest;
        requiresMatchingAppNixpkgs = true;
      };
      checks.${system} = {
        p1-interface = p1InterfaceCheck;
        p4-interface = p4InterfaceCheck;
      };
    };
}
