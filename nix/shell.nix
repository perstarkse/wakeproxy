{
  self,
  inputs,
  pkgs,
  system,
}:
let
  rust = import ./rust-env.nix {inherit inputs pkgs system;};
  git-hooks = inputs.git-hooks.lib.${system}.run {
    src = self;
    hooks = {
      rustfmt.enable = true;
      "cargo-clippy" = {
        enable = true;
        name = "clippy";
        entry = "cargo clippy --workspace --all-targets --all-features -- -D warnings";
        language = "system";
        pass_filenames = false;
      };
      "cargo-test" = {
        enable = true;
        name = "Cargo tests";
        entry = "cargo test";
        language = "system";
        pass_filenames = false;
      };
      "cargo-coverage" = {
        enable = true;
        name = "Coverage summary";
        entry = "cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 90";
        language = "system";
        pass_filenames = false;
      };
    };
  };
in
pkgs.mkShell {
  inputsFrom = [git-hooks];

  packages = with pkgs; [
    rust.rustToolchain
    git
    curl
    cargo-llvm-cov
    rust.llvmTools
  ];

  env = rust.coverageEnv;

  shellHook = ''
    ${git-hooks.shellHook}
  '';
}
