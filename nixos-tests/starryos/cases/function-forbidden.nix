{
  kind = "boot";
  extraModules = [
    ({ lib, ... }:
      {
        assertions = lib.mkForce [ ];
        services.udev.enable = lib.mkForce true;
      })
  ];
}
