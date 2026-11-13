#!/bin/sh
# Development bundle only: separate bundle id (com.lotsmemex.desktop.dev), ad-hoc
# signed, written under target/. Never installs or replaces the shipped Lots MemeX app.
set -eu
cd "$(dirname "$0")/.."
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
# Keep the builder's home and checkout paths (panic locations, dependency sources) out of the binary.
REPO=$(cd ../.. && pwd -P)
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$REPO=/src"
cargo build --release --locked -p memex-shell
APP=target/app/LotsMemeX.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp packaging/Info.plist "$APP/Contents/Info.plist"
# The bundle shows the one workspace version ([workspace.package] version in Cargo.toml).
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
cp assets/icon/LotsMemeX.icns "$APP/Contents/Resources/LotsMemeX.icns"
# 寒蝉拙楷 is embedded in the binary; its SIL OFL 1.1 text and the OpenCC
# dictionaries' Apache-2.0 license (data compiled in) ship beside it.
mkdir -p "$APP/Contents/Resources/Licenses"
cp assets/fonts/OFL-ChillZhuoKai.txt "$APP/Contents/Resources/Licenses/OFL-ChillZhuoKai.txt"
cp ../../mobile/web/assets/opencc/LICENSE-OpenCC.txt "$APP/Contents/Resources/Licenses/LICENSE-OpenCC.txt"
cp target/release/memex-desktop "$APP/Contents/MacOS/memex-desktop"
chmod 755 "$APP/Contents/MacOS/memex-desktop"
codesign --force --sign - --timestamp=none "$APP"
printf '%s\n' "$PWD/$APP"
