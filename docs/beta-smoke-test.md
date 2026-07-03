# Beta smoke test

The manual checklist to run against **every** signed+notarized DMG before it goes
to a tester. It exercises the parts that only break on a *real* install — first
run, permission flow, activation policy, icon identity — none of which reproduce
in `npm run tauri dev` (dev runs unbundled, inherits the terminal's grants, and
uses a different data store). Budget ~5 minutes.

Bundle id: `app.tencalmdigits`. Product name: **TenCalmDigits**.

## 1. Wipe to a true first-run state

Repeated installs leave caches that mask first-run behaviour (stale onboarding
flag, TCC grants, LaunchServices registrations, Finder icon cache). Reset all of
it so the DMG is tested as a brand-new user would see it:

```sh
# Quit any running copy
osascript -e 'quit app "TenCalmDigits"' 2>/dev/null; pkill -f TenCalmDigits 2>/dev/null

# Remove the app and ALL its on-device state
rm -rf /Applications/TenCalmDigits.app
rm -rf ~/.typeassist                       # learning data (the first-run signal)

# Revoke both TCC grants for the id
tccutil reset Accessibility app.tencalmdigits
tccutil reset ListenEvent   app.tencalmdigits

# Eject every stale TenCalmDigits volume, rebuild LaunchServices, clear icon cache
for v in /Volumes/TenCalmDigits*; do hdiutil detach "$v" 2>/dev/null; done
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister \
  -kill -r -domain local -domain user
sudo rm -rf /Library/Caches/com.apple.iconservices.store; qlmanage -r cache; killall Finder Dock
```

> **Note on the localStorage flag.** `ta.onboarded` lives in the WebKit data
> store, which survives the steps above. That's fine — the app reconciles it
> against the backend `is_first_run` on launch (data + Accessibility), so a wiped
> `~/.typeassist` + revoked grant is treated as a first run regardless. If you
> want to wipe it too: `rm -rf ~/Library/WebKit/app.tencalmdigits` (not required).

## 2. Install from /Applications

Mount the **notarized + stapled** DMG, drag to Applications, and launch **from
/Applications** (not from the mounted volume — test the installed copy, the way a
user runs it). Gatekeeper should open it with no "unidentified developer" block
(that verifies notarization/stapling took).

## 3. Pass checks

Run in order. All must hold.

- [ ] **Onboarding first.** Launch lands on the onboarding flow (name → permission
      → done), NOT the Today screen. (Regression guard: a stale `ta.onboarded`
      must not skip it.)
- [ ] **Tray is honest.** Open the menu-bar icon during onboarding: status reads
      **"Jordan hasn't started yet"** — never "Jordan is active" — because capture
      is deferred until you grant permission. No "Restart"/"Reconnect" item yet.
- [ ] **Accessibility only — no Input Monitoring prompt.** The permission step
      asks for **Accessibility** and nothing else. Granting it is enough; macOS
      does **not** separately prompt for Input Monitoring (Accessibility subsumes
      the listen-only tap), and no second row appears. (If an Input Monitoring row
      *does* appear, that's the conditional fallback — note the macOS version.)
- [ ] **Jordan captures after the grant.** Once Accessibility is on, onboarding
      auto-advances and the tray flips to **"Jordan is active."** Type in any app;
      confirm words are counted (Progress → Statistics shows today's count rising).
- [ ] **Dock is empty in every state.** Check the Dock at: launch, during
      onboarding, with the main window open, with a menu-bar panel open, and idle
      in the menu bar. **No Dock icon, no launch bounce, ever.** If one appears,
      run `lsappinfo info -app app.tencalmdigits | grep -o 'type="[^"]*"'` —
      `UIElement` = a stale LaunchServices tile (see releasing-beta.md § Test-Mac
      gotchas), `Foreground` = a real runtime promotion (a bug — report it).
- [ ] **Yellow hand everywhere.** The Noto hand (🖐, yellow) is the mark on: the
      DMG window + volume icon, Finder/Applications, the About screen, and any
      permission prompt. No rounded-square placeholder, no brown hand. (A brown
      volume icon is Finder's stale icon cache, not the asset — see releasing-beta.md.)
- [ ] **Data survives an upgrade install.** After using it a little (so
      `~/.typeassist/motor_map.json` exists), install a newer DMG over the top
      **without** wiping. It must launch **straight to the app (no onboarding)**
      and the Progress counts/history must be intact — an upgrade must never look
      like a first run or lose recovery data (Principle #6).

Any failure → do not ship; capture the failing check + the relevant one-liner
output and file it.
