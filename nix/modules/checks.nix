{
  self,
  inputs,
  ...
}:
{
  perSystem =
    {
      system,
      pkgs,
      ...
    }:
    let
      rustChecks = import ../checks/rust.nix {
        inherit
          self
          inputs
          system
          pkgs
          ;
      };
      vmChecks = import ../tests/default.nix {
        inherit self pkgs;
        tier = "ci";
      };
    in
    {
      checks = rustChecks // vmChecks;
    };
}
