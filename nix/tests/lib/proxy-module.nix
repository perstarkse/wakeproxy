{
  passwordHashFile,
  upstreamHost ? "target",
  wolBroadcastIp,
  listenAddress ? "0.0.0.0",
  keepAwake ? { },
  extra ? { },
}:
{
  enable = true;
  inherit listenAddress upstreamHost wolBroadcastIp;
  port = 8091;
  upstreamPort = 3000;
  wolMac = "aa:bb:cc:dd:ee:ff";
  wolBroadcastPort = 4009;
  wakeTimeout = 20;
  pollInterval = 1;
  readyCacheTtl = 1;
  wakePollIntervalMs = 500;
  inherit passwordHashFile;
  cookieSecure = false;
  loginMaxFailures = 1000;
}
// extra
// {
  keepAwake = keepAwake;
}
