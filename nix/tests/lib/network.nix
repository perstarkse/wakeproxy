# NixOS VM tests assign eth1 addresses as 192.168.1.<nodeNumber>, where
# nodeNumber is the node's index in alphabetical order of `nodes` keys
# (see nixos/lib/testing/network.nix). Custom networking.interfaces.eth1
# addresses are overwritten by the test harness.
{
  nodeIp = nodeName: nodeNames:
    let
      sorted = builtins.sort builtins.lessThan nodeNames;
      numbered = builtins.map (i: {
        name = builtins.elemAt sorted i;
        number = i + 1;
      }) (builtins.genList (i: i) (builtins.length sorted));
      match =
        builtins.head (builtins.filter (x: x.name == nodeName) numbered);
    in
    "192.168.1.${toString match.number}";

  mkEth1Node =
    {
      extraConfig ? {},
    }:
    {
      ...
    }:
    {
      networking.useDHCP = false;
    }
    // extraConfig;
}
