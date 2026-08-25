{
  pkgs,
  wakeproxy,
  archiveName ? "wakeproxy-linux-x86_64.tar.gz",
}:
pkgs.runCommand "wakeproxy-release-archive"
  {
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.gnutar
    ];
  }
  ''
    set -eu

    stage="$(mktemp -d)"
    install -m 0755 "${wakeproxy}/bin/wakeproxy" "$stage/wakeproxy"
    cp "${./../../README.md}" "$stage/README.md"
    cp "${./../../LICENSE}" "$stage/LICENSE"
    cp "${./../../examples/wakeproxy.service}" "$stage/wakeproxy.service"

    mkdir -p "$out"
    tar -C "$stage" -czf "$out/${archiveName}" .

    # sha256 sidecar with the bare filename, matching the pre-Nix release layout.
    cd "$out"
    sha256sum "${archiveName}" > "${archiveName}.sha256"
  ''
