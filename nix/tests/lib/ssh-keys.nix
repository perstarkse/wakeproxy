{pkgs}: let
  privateKey = pkgs.writeText "wakeproxy-keep-awake-test-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACClbrLeIng+fxAnDxFMQeyzajxLU0OFKv2oDiCAP02x6gAAAKCYkOzvmJDs
    7wAAAAtzc2gtZWQyNTUxOQAAACClbrLeIng+fxAnDxFMQeyzajxLU0OFKv2oDiCAP02x6g
    AAAEBbiHT2aCwQG1c3JfcQNrMAQZTFnQ+79u6LpwrXgPw49KVust4ieD5/ECcPEUxB7LNq
    PEtTQ4Uq/agOIIA/TbHqAAAAGXdha2Vwcm94eS1rZWVwLWF3YWtlLXRlc3QBAgME
    -----END OPENSSH PRIVATE KEY-----
  '';
  publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKVust4ieD5/ECcPEUxB7LNqPEtTQ4Uq/agOIIA/TbHq wakeproxy-keep-awake-test";
in {
  inherit privateKey publicKey;
  proxyActivationScript = ''
    mkdir -p /var/lib/wake-proxy
    cp ${privateKey} /var/lib/wake-proxy/keep-awake-ssh-key
    chmod 600 /var/lib/wake-proxy/keep-awake-ssh-key
    chown wake-proxy /var/lib/wake-proxy/keep-awake-ssh-key
  '';
}
