{
  kind = "assert";
  command = "hello";
  expectedStatus = 0;
  expectedOutput = "Hello, world!";
  expectPass = true;
  packages = pkgs: [ pkgs.hello ];
}
