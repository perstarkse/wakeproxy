{
  inputs,
  pkgs,
  system,
}: let
  fenixPkgs = inputs.fenix.packages.${system};
  rustToolchain = fenixPkgs.combine [
    fenixPkgs.stable.rustc
    fenixPkgs.stable.cargo
    fenixPkgs.stable.rustfmt
    fenixPkgs.stable.clippy
  ];
  llvmTools = fenixPkgs.stable."llvm-tools-preview";
  rustTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget or pkgs.stdenv.hostPlatform.config;
  llvmToolsBin = "${llvmTools}/lib/rustlib/${rustTarget}/bin";
in {
  inherit fenixPkgs rustToolchain llvmTools llvmToolsBin rustTarget;
  coverageEnv = {
    LLVM_COV = "${llvmToolsBin}/llvm-cov";
    LLVM_PROFDATA = "${llvmToolsBin}/llvm-profdata";
  };
}
