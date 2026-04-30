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
      until_file="$state_dir/$unit_name.until"
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

      now="$(date +%s)"
      requested_until="$((now + seconds))"

      if [ -f "$pid_file" ]; then
        existing_pid="$(cat "$pid_file" 2>/dev/null || true)"
        existing_until="$(cat "$until_file" 2>/dev/null || echo 0)"
        case "$existing_until" in
          *[![:digit:]]*) existing_until=0 ;;
        esac

        if [ -n "$existing_pid" ] && kill -0 "$existing_pid" 2>/dev/null; then
          if [ "$existing_until" -ge "$requested_until" ]; then
            printf 'existing keep-awake lease remains active until %s; requested until %s\n' "$existing_until" "$requested_until" >> "$log_file"
            exit 0
          fi

          kill "$existing_pid" 2>/dev/null || true
        fi
        rm -f "$pid_file" "$until_file"
      fi

      nohup systemd-inhibit \
        --what=idle:sleep \
        --mode=block \
        --why="wakeproxy remote session" \
        sleep "$seconds" \
        >"$log_file" 2>&1 &
      inhibit_pid="$!"
      printf '%s\n' "$inhibit_pid" > "$pid_file"
      printf '%s\n' "$requested_until" > "$until_file"
    '';

  }