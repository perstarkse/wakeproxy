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
      state_dir="''${WAKEPROXY_KEEP_AWAKE_STATE_DIR:-/tmp}"
      pid_file="$state_dir/$unit_name.pid"
      log_file="$state_dir/$unit_name.log"
 
      case "$seconds" in
        *[![:digit:]]*)
          echo "duration must be a positive integer" >&2
          exit 1
          ;;
      esac
 
      case "$max_seconds" in
        *[![:digit:]]*)
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
      mkdir -p "$state_dir"
 
      if [ -f "$pid_file" ]; then
        existing_pid="$(cat "$pid_file" 2>/dev/null || true)"
        if [ -n "$existing_pid" ] && kill -0 "$existing_pid" 2>/dev/null; then
          kill "$existing_pid" 2>/dev/null || true
        fi
        rm -f "$pid_file"
      fi
 
      nohup systemd-inhibit \
        --what=idle:sleep \
        --mode=block \
        --why="wakeproxy remote session" \
        sleep "$seconds" \
        >"$log_file" 2>&1 &
      inhibit_pid="$!"
      printf '%s\n' "$inhibit_pid" > "$pid_file"
    '';

  }