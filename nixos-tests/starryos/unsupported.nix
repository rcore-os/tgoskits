# Fail-closed command-channel nixosTest contract for StarryOS.
{ lib, pkgs, launcher, starryMachine }:
let
  boot = import ./boot.nix { inherit lib pkgs launcher starryMachine; };
in
{
  inherit (boot) patterns;
  contract = boot.contract // {
    name = "starry-nixos-unsupported";
    testScript = ''
      import importlib.util
      spec = importlib.util.spec_from_file_location("starry_machine", ${builtins.toJSON (toString starryMachine)})
      assert spec is not None and spec.loader is not None
      starry_machine = importlib.util.module_from_spec(spec)
      spec.loader.exec_module(starry_machine)
      wrap_machine = starry_machine.wrap_machine

      machine = wrap_machine(create_machine(${builtins.toJSON launcher}, name="starry-nixos-unsupported"))
      machine.succeed("true")
    '';
  };
}
