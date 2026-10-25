#!/bin/bash
# ScreenLink shell-agent 部署（内置 MSYS_NO_PATHCONV，Git Bash 下直接跑）
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL="*"
A="192.168.10.23:36435"
APK="${1:-android/app/build/outputs/apk/debug/app-debug.apk}"
SHA="${2:-$(python -c "
import json, os, hashlib
d = json.load(open(os.path.join(os.environ['APPDATA'],'screenlink','pc_token.json')))
print(hashlib.sha256(d['token'].encode()).hexdigest())")}"
set -e
adb -s "$A" push "$APK" /sdcard/Download/sl-agent.apk
adb -s "$A" shell 'pkill -f "[A]gentMainKt"; sleep 0.5' || true
adb -s "$A" shell 'CLASSPATH=/sdcard/Download/sl-agent.apk setsid app_process / dev.screenlink.companion.agent.AgentMainKt 27188 /sdcard/Download/ScreenLink '"$SHA"' > /data/local/tmp/sl-agent.log 2>&1 &'
sleep 4
adb -s "$A" shell 'pgrep -f "[A]gentMainKt" && head -2 /data/local/tmp/sl-agent.log'
