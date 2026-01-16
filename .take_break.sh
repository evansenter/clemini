#!/bin/bash
msg="Hey Evan, it's time to take a short break! Stretch your back and look at something 20 feet away for 20 seconds."

if [[ "$OSTYPE" == "darwin"* ]]; then
  # macOS
  osascript -e "display notification \"$msg\" with title \"Break Time\""
else
  # Linux
  export DISPLAY=:0
  export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
  notify-send "Break Time" "$msg" -i clock || true
fi

echo "$msg" | wall
