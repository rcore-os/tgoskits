{
  kind = "boot";
  extraModules = [
    ({ lib, ... }:
      {
        boot.kernel.sysctl."kernel.pid_max" = lib.mkOverride 10 1;
      })
  ];
}
