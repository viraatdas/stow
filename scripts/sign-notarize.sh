#!/usr/bin/env bash
# Build StowAgent + extension, embed the Developer ID provisioning profiles
# (which carry the App Groups entitlement), sign Developer ID, notarize, staple,
# install. This is the combination that should make addDomain work: notarized
# (Gatekeeper passes) AND app-group present (extension is a valid provider).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

ID="Developer ID Application: Viraat Das (3C4383262W)"
APP_PROFILE="/tmp/stow_ai_exla_stow.provisionprofile"
FP_PROFILE="/tmp/stow_ai_exla_stow_fileprovider.provisionprofile"
APP="build/Build/Products/Release/StowAgent.app"
APPEX="$APP/Contents/PlugIns/StowFileProvider.appex"

echo "[1/8] generate project"
rm -rf Stow.xcodeproj build
xcodegen generate >/dev/null

echo "[2/8] build unsigned"
xcodebuild -project Stow.xcodeproj -scheme StowAgent -configuration Release \
  -derivedDataPath build build \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO >/tmp/xcb.log 2>&1 \
  || { echo "BUILD FAILED"; tail -20 /tmp/xcb.log; exit 1; }

echo "[3/8] embed provisioning profiles"
cp "$FP_PROFILE" "$APPEX/Contents/embedded.provisionprofile"
cp "$APP_PROFILE" "$APP/Contents/embedded.provisionprofile"

echo "[4/8] sign extension"
codesign --force --options runtime --timestamp \
  --entitlements apps/StowFileProvider/StowFileProvider.entitlements \
  --sign "$ID" "$APPEX"

echo "[5/8] sign app"
codesign --force --options runtime --timestamp \
  --entitlements apps/StowAgent/StowAgent.entitlements \
  --sign "$ID" "$APP"

echo "[6/8] verify signature"
codesign --verify --deep --strict "$APP" && echo "  codesign OK"

echo "[7/8] notarize"
ditto -c -k --keepParent "$APP" /tmp/StowAgent-signed.zip
xcrun notarytool submit /tmp/StowAgent-signed.zip --keychain-profile stow-notary --wait \
  2>&1 | grep -iE 'id:|status:' | tail -4

echo "[8/8] staple + install"
xcrun stapler staple "$APP"
killall StowAgent 2>/dev/null || true
rm -rf /Applications/StowAgent.app
cp -R "$APP" /Applications/StowAgent.app
echo "DONE"
