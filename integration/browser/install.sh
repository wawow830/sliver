#!/bin/sh
# Install the Sliver browser integration for the current user, from this checkout.
#
#   integration/browser/install.sh [CHROMIUM_EXTENSION_ID]
#
# - registers the native-messaging host for Firefox and (if an extension id is given) for
#   Chromium, Chrome and Brave, pointing at integration/browser/host/sliver-browser-host;
# - writes ready-to-load extension folders to integration/browser/build/{firefox,chromium}.
# Then load the extension: Firefox: about:debugging → This Firefox → Load Temporary Add-on →
# build/firefox/manifest.json. Chromium: chrome://extensions → Developer mode → Load unpacked →
# build/chromium (copy the id it shows and run this script again with it).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
host="$here/host/sliver-browser-host"
name=io.github.wawow830.sliver_browser
cfg=${XDG_CONFIG_HOME:-$HOME/.config}

for b in firefox chromium; do
	mkdir -p "$here/build/$b"
	cp "$here/extension/background.js" "$here/build/$b/"
	cp "$here/extension/manifest.$b.json" "$here/build/$b/manifest.json"
done

mkdir -p "$HOME/.mozilla/native-messaging-hosts"
sed "s|/usr/libexec/sliver-browser-host|$host|" "$here/native/$name.firefox.json" \
	>"$HOME/.mozilla/native-messaging-hosts/$name.json"
echo "Firefox: host registered"

if [ $# -ge 1 ]; then
	for d in chromium google-chrome BraveSoftware/Brave-Browser; do
		mkdir -p "$cfg/$d/NativeMessagingHosts"
		sed -e "s|/usr/libexec/sliver-browser-host|$host|" -e "s|EXTENSION_ID|$1|" \
			"$here/native/$name.chromium.json" >"$cfg/$d/NativeMessagingHosts/$name.json"
	done
	echo "Chromium, Chrome, Brave: host registered for extension $1"
fi
