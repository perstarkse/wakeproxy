{
  self,
  pkgs,
  inputs,
  system,
}:
let
  rust = import ../rust-env.nix { inherit inputs pkgs system; };
in
{
  rust = pkgs.rustPlatform.buildRustPackage {
    pname = "wakeproxy-rust-checks";
    version = self.packages.${system}.default.version;
    src = self;
    cargoLock.lockFile = self + "/Cargo.lock";

    rustc = rust.fenixPkgs.stable.rustc;
    cargo = rust.fenixPkgs.stable.cargo;

    nativeBuildInputs = [
      rust.rustToolchain
      pkgs.cargo-llvm-cov
      rust.llvmTools
    ];

    doCheck = true;

    installPhase = ''
      runHook preInstall
      mkdir -p "$out"
      runHook postInstall
    '';

    preCheck = ''
      export LLVM_COV="${rust.coverageEnv.LLVM_COV}"
      export LLVM_PROFDATA="${rust.coverageEnv.LLVM_PROFDATA}"
    '';

    checkPhase = ''
      runHook preCheck

      cargo clippy --workspace --all-targets --all-features -- -D warnings

      cargo test --workspace

      cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 90

      runHook postCheck
    '';

    meta = self.packages.${system}.default.meta;
  };
}
