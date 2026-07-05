# Releasing the signed beta DMG

The distributed beta is a **Developer ID**–signed, **notarized**, **Universal
Binary** (arm64 + x86_64, since v0.2.0) DMG (distinct from the local self-signed
dev build — see `macos-signing.md`). Bundle identity: `app.tencalmdigits`, product
name **TenCalmDigits**. Installs natively on Apple Silicon **and** Intel Macs.

> **Why the id matters.** A product rename (TypeAssist → TenCalmDigits) that kept
> one `CFBundleIdentifier` across many on-disk bundles corrupted TCC/LaunchServices
> name resolution and produced an endless Accessibility-prompt loop. The id is now
> `app.tencalmdigits`; **never ship two product names under one bundle id.**

## Prerequisites (in the builder's keychain, never the repo)

- **Developer ID Application** certificate: `Developer ID Application: Soumyo Sinha (XV484C8C6K)`.
- A **notarytool keychain profile** holding the Apple ID credentials. Create once:
  ```sh
  xcrun notarytool store-credentials "<profile-name>" \
    --apple-id "<apple-id>" --team-id XV484C8C6K
  # then use --keychain-profile "<profile-name>" below
  ```

## Build (same recipe each time) — Universal Binary (arm64 + x86_64)

`just` is **not** installed on the build Mac — use the plain shell steps.

Since **v0.2.0** the beta ships as a **Universal Binary** so it installs natively
on both Apple Silicon and Intel Macs. Two non-obvious mechanics drive the sidecar
step below — read them before touching it:

- **CLT-only cross-compile.** This build Mac has **Command Line Tools, not full
  Xcode**, so the one-shot `swift build --arch arm64 --arch x86_64` fails
  (`xcbuild`/`XCBuild.framework` is absent). Build each slice **separately** into
  isolated scratch dirs — arm64 natively, x86_64 by overriding the target triple —
  then `lipo -create`. The `__TEXT,__info_plist` friendly-name section survives in
  both slices.
- **Tauri's universal build needs THREE staged sidecar files at once.** Tauri
  compiles each arch in a separate cargo sub-build (each validates its **own**
  per-arch sidecar name), then the bundler copies in a **fat** `-universal-` one —
  Tauri does *not* lipo sidecars itself. So all three must be present:
  `…-aarch64-apple-darwin` (thin arm64), `…-x86_64-apple-darwin` (thin x86_64),
  and `…-universal-apple-darwin` (fat). Missing a per-arch one → the cargo
  build-script fails; missing the fat one → the bundler fails.

```sh
# 1. Build the universal Swift sidecar and stage the THREE files, each signed
#    Developer ID + hardened runtime. (Tauri re-signs the nested copy during
#    bundling, but stage them clean.)
SIDE=apps/tauri/src-tauri/sidecars
DEVID="Developer ID Application: Soumyo Sinha (XV484C8C6K)"

# 1a. Build each arch into its own scratch dir (CLT-only cross-compile).
cd adapters/macos
swift build -c release --scratch-path .build-arm64
swift build -c release --scratch-path .build-x86 \
  -Xswiftc -target -Xswiftc x86_64-apple-macosx13.0 \
  -Xcc     -target -Xcc     x86_64-apple-macosx13.0 \
  -Xlinker -arch   -Xlinker x86_64
cd -

# 1b. lipo the fat binary, then extract the two thin per-arch slices from it
#     (so all three come from identical current source).
mkdir -p "$SIDE"
lipo -create \
  adapters/macos/.build-arm64/arm64-apple-macosx/release/typeassist-input-macos \
  adapters/macos/.build-x86/arm64-apple-macosx/release/typeassist-input-macos \
  -output "$SIDE/typeassist-input-macos-universal-apple-darwin"
lipo "$SIDE/typeassist-input-macos-universal-apple-darwin" -thin arm64 \
  -output "$SIDE/typeassist-input-macos-aarch64-apple-darwin"
lipo "$SIDE/typeassist-input-macos-universal-apple-darwin" -thin x86_64 \
  -output "$SIDE/typeassist-input-macos-x86_64-apple-darwin"
rm -rf adapters/macos/.build-arm64 adapters/macos/.build-x86

# 1c. Sign all three (Developer ID + hardened runtime + stable identifier).
for f in "$SIDE"/typeassist-input-macos-{aarch64,x86_64,universal}-apple-darwin; do
  codesign --force --options runtime --sign "$DEVID" \
    --identifier "typeassist-input-macos" "$f"
done
# sanity: both real slices in the fat one
lipo -info "$SIDE/typeassist-input-macos-universal-apple-darwin"   # → x86_64 arm64

# 2. Build the signed universal .app + .dmg with the release config overlay.
#    --target universal-apple-darwin makes Tauri build+lipo both arches.
#    (The overlay deep-merges over tauri.conf.json: Developer ID identity +
#    entitlements.plist + minimumSystemVersion 13.0; base bundle.icon inherited.)
cd apps/tauri && npm run tauri build -- \
  --target universal-apple-darwin \
  --config src-tauri/tauri.release.conf.json && cd -
```

Outputs (note the `universal-apple-darwin` target dir and the `universal` arch token):
- `target/universal-apple-darwin/release/bundle/macos/TenCalmDigits.app`
- `target/universal-apple-darwin/release/bundle/dmg/TenCalmDigits_0.2.0_universal.dmg`

Verify **both** the app binary and the bundled sidecar are fat before continuing:
```sh
APP=target/universal-apple-darwin/release/bundle/macos/TenCalmDigits.app
lipo -info "$APP/Contents/MacOS/typeassist-app"          # → x86_64 arm64
lipo -info "$APP/Contents/MacOS/typeassist-input-macos"  # → x86_64 arm64
```

### Version-stamp the DMG volume name (do this BEFORE notarize/staple)

Tauri names the DMG **volume** after `productName` alone ("TenCalmDigits") — every
release mounts under the same name, so Finder reuses an earlier mount's cached
volume icon (the wrong-hand gotcha below) and testers can't tell versions apart.
There is no Tauri config for the volume name, so re-stamp it post-build. Bump
`VOL` to match this release's version; run before notarizing so notarization
covers the final artifact:

```sh
DMG="target/universal-apple-darwin/release/bundle/dmg/TenCalmDigits_0.2.0_universal.dmg"
VOL="TenCalmDigits 0.2.0"   # ← must match tauri.conf.json "version" each release

# Convert the read-only build DMG to read-write, rename its volume, convert back
# to compressed (UDZO) at the SAME path so the notarize/staple commands below are
# unchanged. The volume's .VolumeIcon.icns rides along, now cached under the new
# name — which also clears the stale volume-icon symptom for good.
hdiutil convert "$DMG" -format UDRW -o /tmp/tcd-rw.dmg
hdiutil attach /tmp/tcd-rw.dmg -nobrowse -noverify -mountpoint /tmp/tcd-mnt
diskutil rename /tmp/tcd-mnt "$VOL"
# Hide the volume-icon file so the DMG window is a clean two items (the .app +
# Applications). It's already a dotfile, but bundle_dmg.sh flags .DS_Store hidden
# and leaves .VolumeIcon.icns unflagged — match them (UF_HIDDEN + the Finder
# invisible bit). NOTE: this keeps it out of a DEFAULT Finder window (what testers
# have); a Finder with "Show hidden files" (Cmd+Shift+.) ON still reveals it — no
# root volume-icon file can hide from that, so verify a clean window with hidden
# files OFF.
chflags hidden /tmp/tcd-mnt/.VolumeIcon.icns
SetFile -a V /tmp/tcd-mnt/.VolumeIcon.icns 2>/dev/null || true
hdiutil detach /tmp/tcd-mnt
rm -f "$DMG"
hdiutil convert /tmp/tcd-rw.dmg -format UDZO -o "$DMG"
rm -f /tmp/tcd-rw.dmg
# Confirm: mounting the DMG now shows the volume as "TenCalmDigits 0.2.0".
```

Verify before notarizing:
```sh
codesign --verify --deep --strict --verbose=2 \
  target/universal-apple-darwin/release/bundle/macos/TenCalmDigits.app   # → valid, satisfies DR
```
`spctl -a -vv` will say **"Unnotarized Developer ID"** until the next step — expected.

## Notarize + staple (required for distribution)

The Tauri build **skips notarization** (no `APPLE_*` env vars) — do it explicitly
on the DMG, then staple so it validates offline:

```sh
DMG="target/universal-apple-darwin/release/bundle/dmg/TenCalmDigits_0.2.0_universal.dmg"
xcrun notarytool submit "$DMG" --keychain-profile "<profile-name>" --wait
xcrun stapler staple "$DMG"
# confirm:
spctl -a -vv -t open --context context:primary-signature \
  "$DMG"   # → accepted, source=Notarized Developer ID
```

## First-install cleanup on a Mac that ran an OLD-id build

The new `app.tencalmdigits` id is clean on any fresh Mac. But a Mac that already
ran a `app.typeassist`-era build carries stale TCC/LaunchServices entries (the
prompt may still show the old "TypeAssist" name). Clear them once:

```sh
# stale bundles still claiming the old id
osascript -e 'tell app "Finder" to empty trash'          # removes ~/.Trash/*TypeAssist.app
for v in /Volumes/dmg.* /Volumes/TenCalmDigits; do hdiutil detach "$v" 2>/dev/null; done

# rebuild the LaunchServices database (drops duplicate name registrations)
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister \
  -kill -r -domain local -domain user -domain system

# clear grants keyed to the OLD id, then reinstall the notarized DMG and grant once
tccutil reset Accessibility app.typeassist
tccutil reset ListenEvent   app.typeassist
rm -rf /Applications/TenCalmDigits.app
```

## Test-Mac gotchas (repeated installs)

Cycling many DMGs of the **same** bundle id / volume name on one Mac leaves two
kinds of stale cache that produce confusing symptoms. Neither is a bug in the
build — both are macOS caches keyed to a name that every version shares.

### A Dock icon appears despite `LSUIElement=true`

The app is a menu-bar agent (`LSUIElement` in the bundle plist + a runtime
`set_activation_policy(Accessory)` re-asserted on `RunEvent::Ready` and main-window
focus). If a Dock icon still shows, it's almost always a **stale LaunchServices
registration** from an earlier install (repeated same-id installs accumulate
registrations; a pre-`LSUIElement` one can win the launch). Diagnose and clear:

```sh
# How many registrations claim the id? (many = stale ones present)
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister \
  -dump | grep -c 'app.tencalmdigits'

# With the app running, is it actually promoted, or just a stale Dock tile?
lsappinfo info -app app.tencalmdigits | grep -o 'type="[^"]*"'
#   type="UIElement"  → accessory (correct); the tile is a stale registration
#   type="Foreground" → a real runtime promotion (report it)

# Rebuild the LaunchServices DB, then reinstall the notarized DMG:
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister \
  -kill -r -domain local -domain user
rm -rf /Applications/TenCalmDigits.app
```

**Confirmed not a code regression (2026-07-04, v1.5).** On the reporting Mac
`lsappinfo info -app app.tencalmdigits` returned `type="UIElement"` while the Dock
tile was showing — i.e. the activation policy is correct and the tile is a stale
LaunchServices registration, not a runtime `Foreground` promotion. The fix is the
cache rebuild above (plus the version-stamped volume name, which stops new stale
registrations accumulating under one shared name). Re-open **only** if a future
`lsappinfo` reads `type="Foreground"` — that would be a real runtime promotion to
fix in `set_activation_policy`.

### The DMG volume icon is the wrong hand (e.g. brown, not yellow)

The DMG's volume icon (`.VolumeIcon.icns`) is **byte-identical** to the app icon
(the yellow Noto hand) — there is nothing to regenerate. Finder caches volume
icons by **volume name**, and every DMG mounts as "TenCalmDigits", so it reuses
an older mount's cached icon. Clear it:

```sh
sudo rm -rf /Library/Caches/com.apple.iconservices.store
qlmanage -r cache
killall Finder Dock
```

A Mac that never mounted an earlier DMG renders it correctly. To sidestep the
cache for testers entirely, give each release a version-stamped DMG **volume
name** (so Finder never reuses a stale icon) — not yet done.
