# Releasing the signed beta DMG

The distributed beta is a **Developer ID**–signed, **notarized** DMG (distinct
from the local self-signed dev build — see `macos-signing.md`). Bundle identity:
`app.tencalmdigits`, product name **TenCalmDigits**.

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

## Build (same recipe each time)

`just` is **not** installed on the build Mac — use the plain shell steps.

```sh
# 1. Rebuild the Swift sidecar, stage it under the Tauri target triple, sign it.
#    (Tauri re-signs the nested copy with Developer ID + hardened runtime during
#    bundling, so the stage-time signature is transient — but keep it clean.)
cd adapters/macos && swift build -c release && cd -
cp adapters/macos/.build/release/typeassist-input-macos \
   apps/tauri/src-tauri/sidecars/typeassist-input-macos-aarch64-apple-darwin
codesign --force --options runtime \
  --sign "Developer ID Application: Soumyo Sinha (XV484C8C6K)" \
  --identifier "typeassist-input-macos" \
  apps/tauri/src-tauri/sidecars/typeassist-input-macos-aarch64-apple-darwin

# 2. Build the signed .app + .dmg with the release config overlay.
#    (Deep-merges over tauri.conf.json: Developer ID identity + entitlements.plist
#    + minimumSystemVersion 13.0. The base bundle.icon set is inherited.)
cd apps/tauri && npm run tauri build -- --config src-tauri/tauri.release.conf.json && cd -
```

Outputs:
- `target/release/bundle/macos/TenCalmDigits.app`
- `target/release/bundle/dmg/TenCalmDigits_0.1.0_aarch64.dmg`

Verify before notarizing:
```sh
codesign --verify --deep --strict --verbose=2 \
  target/release/bundle/macos/TenCalmDigits.app          # → valid, satisfies DR
```
`spctl -a -vv` will say **"Unnotarized Developer ID"** until the next step — expected.

## Notarize + staple (required for distribution)

The Tauri build **skips notarization** (no `APPLE_*` env vars) — do it explicitly
on the DMG, then staple so it validates offline:

```sh
xcrun notarytool submit target/release/bundle/dmg/TenCalmDigits_0.1.0_aarch64.dmg \
  --keychain-profile "<profile-name>" --wait
xcrun stapler staple target/release/bundle/dmg/TenCalmDigits_0.1.0_aarch64.dmg
# confirm:
spctl -a -vv -t open --context context:primary-signature \
  target/release/bundle/dmg/TenCalmDigits_0.1.0_aarch64.dmg   # → accepted, source=Notarized Developer ID
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
