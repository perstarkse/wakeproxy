{
  projectRootFile = "flake.nix";

  programs.rustfmt = {
    enable = true;
    edition = "2024";
  };

  programs.nixfmt.enable = true;
}
