{
  lib,
  inputs,
  config,
  ...
}:
{
  options.wakeproxy.sccacheOverlay = lib.mkOption {
    type = lib.types.raw;
    default =
      if builtins.pathExists ./../../sccache.local.nix then
        [ (import ./../../sccache.local.nix) ]
      else
        [ ];
    defaultText = lib.literalExpression "[]";
    description = ''
      Opt-in sccache overlay for local nix build/cache of Rust derivations.
      Activate with `git add -N sccache.local.nix` (nix only sees git-staged
      files); absent for anyone without the file.
    '';
  };

  config.perSystem =
    { system, ... }:
    {
      _module.args.pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = config.wakeproxy.sccacheOverlay;
      };
    };
}
