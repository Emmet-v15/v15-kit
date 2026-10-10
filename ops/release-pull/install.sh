#!/usr/bin/env bash
# Installs or updates release-pull on a server using a single app's config,
# and can give that app's CI an SSH key restricted to installing its releases:
#
#   bash ops/release-pull/install.sh <ssh host> <app config .json> [<ci public key file>]
#
# The app name matches the config file name (myapp.json -> myapp). Each app
# keeps its config in its own repo. On Windows, run it through Git Bash from
# PowerShell (a plain `bash` there might be WSL):
#   & "C:\Program Files\Git\bin\bash.exe" ops/release-pull/install.sh <host> ..\myapp\ops\myapp.json "$HOME/.ssh/myapp-ci.pub"
#
# Copies release-pull to /usr/local/bin and the config to /etc/release-pull/.
# When a public key is given, it adds it to the SSH user's authorized_keys as
# `command="/usr/local/bin/release-pull <app>",restrict`, replacing any
# previous key for that app. For private repositories, CI sends a short-lived
# token on each run ("token": "stdin"), so the server stores no credentials.
set -euo pipefail
HOST=${1:?ssh host}
CONF=${2:?app config .json}
KEY=${3:-}
[ -f "$CONF" ] || { echo "no such file: $CONF"; exit 1; }
APP=$(basename "$CONF" .json)
[[ "$APP" =~ ^[a-z0-9][a-z0-9-]*$ ]] || { echo "not an app name: $APP (the config's file name)"; exit 1; }
python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$CONF" 2>/dev/null \
  || node -e "JSON.parse(require('fs').readFileSync(process.argv[1], 'utf8'))" "$CONF"
SCRIPT="$(cd "$(dirname "$0")" && pwd)/release-pull"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp "$SCRIPT" "$tmp/release-pull"
cp "$CONF" "$tmp/$APP.json"
tar czf - -C "$tmp" release-pull "$APP.json" | ssh "$HOST" "set -e
  d=\$(mktemp -d); tar xzf - -C \"\$d\"
  sudo install -m 755 -o root -g root \"\$d/release-pull\" /usr/local/bin/release-pull
  sudo install -d -m 755 /etc/release-pull
  sudo install -m 644 -o root -g root \"\$d/$APP.json\" /etc/release-pull/$APP.json
  rm -rf \"\$d\"
  sudo touch /var/log/release-pull.log"
echo "release-pull and /etc/release-pull/$APP.json installed on $HOST"

if [ -n "$KEY" ]; then
  PUB=$(tr -d '\r' < "$KEY")
  # Exactly one key, on one line: a second line would be authorized
  # without the restriction.
  [[ "$PUB" =~ ^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp[0-9]+)\ [A-Za-z0-9+/]+=*(\ [[:print:]]*)?$ ]] \
    || { echo "$KEY is not a single-line public key"; exit 1; }
  LINE="command=\"/usr/local/bin/release-pull $APP\",restrict $PUB"
  printf '%s\n' "$LINE" | ssh "$HOST" "set -e
    f=~/.ssh/authorized_keys; cp \"\$f\" \"\$f.bak-release-pull\"
    grep -v -F 'command=\"/usr/local/bin/release-pull $APP\"' \"\$f.bak-release-pull\" > \"\$f.new\" || true
    cat >> \"\$f.new\"; chmod 600 \"\$f.new\"; mv \"\$f.new\" \"\$f\""
  # Delete the backup once a fresh login confirms the new file works; if
  # left behind, it would retain every key removed since.
  ssh "$HOST" 'rm -f ~/.ssh/authorized_keys.bak-release-pull'
  echo "CI key for $APP added on $HOST (it can only run release-pull $APP)"
fi
