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
    {
      devShells.default = import ../shell.nix {
        inherit
          self
          inputs
          pkgs
          system
          ;
      };
    };
}
