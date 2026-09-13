{
  kind = "assert";
  extraModules = [ ../modules/hello-tmpfiles.nix ];
  command = "cat /etc/starry-nixos/hello-tmpfiles";
  expectedStatus = 0;
  expectedOutput = "tmpfiles-ok";
  expectPass = true;
}
