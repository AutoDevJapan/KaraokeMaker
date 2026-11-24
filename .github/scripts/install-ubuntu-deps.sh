#!/usr/bin/env bash
# Use bounded Ubuntu mirrors with the distribution's existing signing keys.
# Keep the hosted runner's source files and third-party repositories unchanged.
set -euo pipefail
if (( $# == 0 )); then
  printf 'At least one Ubuntu package is required\n' >&2
  exit 2
fi
for package in "$@"; do
  [[ "$package" =~ ^[a-z0-9][a-z0-9+.-]*$ ]] || {
    printf 'Invalid Ubuntu package name: %s\n' "$package" >&2
    exit 2
  }
done
source /etc/os-release
[[ "$ID" == ubuntu ]] || { printf 'Ubuntu runner required\n' >&2; exit 2; }
case "$VERSION_CODENAME" in
  jammy|noble|resolute) ;;
  *) printf 'Unsupported Ubuntu suite: %s\n' "$VERSION_CODENAME" >&2; exit 2 ;;
esac
keyring=/usr/share/keyrings/ubuntu-archive-keyring.gpg
[[ -r "$keyring" ]] || { printf 'Ubuntu archive keyring is missing\n' >&2; exit 2; }
sources="$(mktemp)"
trap 'rm -- "$sources"' EXIT
chmod 644 "$sources"
apt_options=(
  -o "Dir::Etc::sourcelist=$sources"
  -o Dir::Etc::sourceparts=-
  -o APT::Update::Error-Mode=any
  -o Acquire::Retries=0
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
)
root=()
if (( EUID != 0 )); then root=(sudo -n --); fi
downloaded=false
# Wait for the APT lock holder to exit before switching mirrors.
for archive in https://mirrors.edge.kernel.org/ubuntu/ https://archive.ubuntu.com/ubuntu/; do
  : > "$sources"
  for pocket in '' -updates -security -backports; do
    printf 'deb [signed-by=%s] %s %s%s main restricted universe multiverse\n' \
      "$keyring" "$archive" "$VERSION_CODENAME" "$pocket" >> "$sources"
  done
  printf 'Ubuntu dependency archive: %s, suite=%s, architecture=%s\n' \
    "$archive" "$VERSION_CODENAME" "$(dpkg --print-architecture)"
  if ! "${root[@]}" timeout --foreground --kill-after=10s 180s apt-get "${apt_options[@]}" update; then
    printf 'Ubuntu archive index update failed: %s\n' "$archive" >&2
    continue
  fi
  if "${root[@]}" timeout --foreground --kill-after=10s 600s apt-get "${apt_options[@]}" install --yes --download-only "$@"; then
    downloaded=true
    break
  fi
  printf 'Ubuntu package download failed: %s\n' "$archive" >&2
done
[[ "$downloaded" == true ]] || { printf 'All signed Ubuntu archives failed to provide the required packages\n' >&2; exit 100; }

# Apply the downloaded packages once. Never retry an install that may have changed dpkg state.
"${root[@]}" timeout --kill-after=10s 600s apt-get "${apt_options[@]}" install --yes --no-download "$@"
dpkg-query -W -f='${Package}\t${Version}\t${Status}\n' "$@"
