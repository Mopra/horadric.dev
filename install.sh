#!/bin/sh
# Installs Horadric on a Mac, for this user, with no admin rights:
#
#   curl -fsSL https://horadric.dev/install.sh | sh
#
# Downloads the newest release, then lets Horadric install itself: the
# app in ~/Applications, `horadric` linked into ~/.local/bin, opening at
# login, and the Claude Code hooks. A download by curl is not quarantined,
# so Gatekeeper does not stop an app that is not notarized.
set -eu

case "$(uname -s)" in
Darwin) ;;
*)
	echo "This installer is for macOS. Horadric for Windows is at https://horadric.dev"
	exit 1
	;;
esac

url="${HORADRIC_DOWNLOAD:-https://github.com/Mopra/horadric.dev/releases/latest/download/Horadric-macos.tar.gz}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading Horadric..."
curl -fsSL "$url" -o "$tmp/Horadric-macos.tar.gz"
tar -xzf "$tmp/Horadric-macos.tar.gz" -C "$tmp"

if nc -z 127.0.0.1 43117 2>/dev/null; then
	echo "Horadric is running. Quit it from its menu bar icon, keeping the sessions running, then run this again."
	exit 1
fi

"$tmp/Horadric.app/Contents/MacOS/horadric" install
