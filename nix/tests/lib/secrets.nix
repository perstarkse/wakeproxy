{pkgs}: {
  testPassword = "test-password";
  passwordHashFile = pkgs.writeText "wakeproxy-secrets" ''
    WAKEPROXY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$yDDaCqWkOPAHJMrn5KI97w$+sEaPfPEi0Ic2N7Op7r/nOJkdFmZ22/QRdb2QTs/0Vc
    WAKEPROXY_SESSION_SECRET=very-secret-session-key
  '';
}
