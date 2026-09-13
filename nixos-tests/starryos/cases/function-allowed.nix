{
  kind = "boot";
  extraModules = [
    ({ ... }:
      {
        environment.etc."starry-nixos/function-module".text = "function-module-ok\n";
      })
  ];
}
