{pkgs}: let
  runtimePath = pkgs.lib.makeBinPath [
    pkgs.coreutils
    pkgs.systemd
  ];
in
  pkgs.writeShellApplication {
    name = "wakeproxy-keep-awake-target";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.systemd
    ];
    text = ''
      set -eu

      seconds="''${1:?missing duration seconds}"
      max_seconds="''${WAKEPROXY_KEEP_AWAKE_TARGET_MAX_SECS:-14400}"
      unit_name="''${WAKEPROXY_KEEP_AWAKE_TARGET_UNIT:-wakeproxy-keep-awake}"

      case "$seconds" in
        ''\|*[!0-9]*)
          echo "duration must be a positive integer" >&2
          exit 1
          ;;
      esac

      case "$max_seconds" in
        ''\|*[!0-9]*)
          echo "max duration must be a positive integer" >&2
          exit 1
          ;;
      esac

      if [ "$seconds" -le 0 ] || [ "$max_seconds" -le 0 ]; then
        echo "durations must be greater than zero" >&2
        exit 1
      fi

      if [ "$seconds" -gt "$max_seconds" ]; then
        seconds="$max_seconds"
      fi

      export PATH="${runtimePath}:$PATH"

      systemctl stop "$unit_name" 2>/dev/null || true

      exec systemd-run \
        --quiet \
        --unit="$unit_name" \
        --collect \
        systemd-inhibit \
          --what=sleep \
          --mode=block \
          --why="wakeproxy remote session" \
          sleep "$seconds"
    '';
  }
