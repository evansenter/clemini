#!/bin/bash
msg="Hey Evan, it's time to take a short break and stretch!"
export DISPLAY=:0
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus
notify-send "Break Time" "$msg" -i clock || true
echo "$msg" | wall
