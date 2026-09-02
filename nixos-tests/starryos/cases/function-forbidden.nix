{
  kind = "boot";
  extraModules = [
    ({ ... }:
      {
        services.udev.enable = true;
      })
  ];
}
