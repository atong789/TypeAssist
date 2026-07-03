# macOS code signing (local)

> **This page is the LOCAL dev build** (self-signed `TypeAssist Local Signing`,
> not notarized). For the distributed **Developer ID + notarized beta DMG**, see
> [`releasing-beta.md`](releasing-beta.md). The distributed bundle id is
> `app.tencalmdigits`; the local dev config below still uses the same id.

TypeAssist's keystroke capture runs in a Swift **sidecar** (`typeassist-input-macos`)
that needs **Accessibility** (and **Input Monitoring**) permission. macOS ties
that grant to the binary's **code signature**:

- **Ad-hoc signing** (`codesign --sign -`) has no stable anchor, so TCC keys the
  grant on the raw cdhash — which changes on **every rebuild**. Result: the
  permission "resets" and the app keeps re-prompting after each build.
- **A stable signing identity** gives the signature a stable *Designated
  Requirement* (identifier + certificate). TCC keys the grant on that, so it
  **survives rebuilds** — grant once, done.

For a local, on-device app (no distribution), a **self-signed certificate** is
enough. We don't notarize (there's no Apple Developer account and nothing is
distributed); a locally-built app has no quarantine flag, so it launches without
a Gatekeeper prompt regardless.

## The signing identity

- Certificate name (the signing identity): **`TypeAssist Local Signing`**
- Wired into the build:
  - `justfile` → `signing_identity` (used to codesign the sidecar) +
    `sidecar_identifier` = `typeassist-input-macos` (stable, matches what the
    Tauri bundler assigns the nested copy, so the dev and installed sidecars
    share one grant).
  - `apps/tauri/src-tauri/tauri.conf.json` → `bundle.macOS.signingIdentity`
    (signs the `.app` bundle + main binary).

The certificate lives in the builder's **login keychain**, not the repo. To
build on a fresh machine (or after losing the keychain), recreate it with the
same name:

```sh
# 1. Generate a self-signed code-signing cert (LibreSSL/OpenSSL both fine).
cat > /tmp/ta-cert.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = TypeAssist Local Signing
[v3]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
openssl req -x509 -newkey rsa:2048 -keyout /tmp/ta-key.pem -out /tmp/ta-cert.pem \
  -days 3650 -nodes -config /tmp/ta-cert.cnf -extensions v3
openssl pkcs12 -export -inkey /tmp/ta-key.pem -in /tmp/ta-cert.pem \
  -out /tmp/ta-cert.p12 -name "TypeAssist Local Signing" -passout pass:typeassist

# 2. Import into the login keychain, allowing codesign to use the key.
security import /tmp/ta-cert.p12 -k "$HOME/Library/Keychains/login.keychain-db" \
  -P typeassist -A -T /usr/bin/codesign
rm -f /tmp/ta-key.pem /tmp/ta-cert.p12 /tmp/ta-cert.cnf   # don't leave key material around

# 3. Confirm codesign can use it (untrusted self-signed is fine for signing;
#    `find-identity -v` won't list it because it's not *trusted*, but
#    `find-identity` without -v will, and codesign --sign works).
security find-identity -p codesigning | grep "TypeAssist Local Signing"
```

Then `just build-mac` (or `just dev`) signs everything with it. To build ad-hoc
instead (no stable grant), override: `just signing_identity=- build-sidecar`.

## Granting the permission (one time)

The sidecar calls `AXIsProcessTrusted()`, but macOS attributes that check to its
**responsible process — the `TenCalmDigits` app**. So grant **the app**:

System Settings → Privacy & Security → **Accessibility** → add **`TenCalmDigits`**
and toggle it **ON**. That's the entry that matters; once both the app and the
sidecar are stably cert-signed, the app-level grant covers the sidecar. (Under
the old ad-hoc signing this attribution didn't hold — granting the app alone
kept re-prompting — which is the bug stable signing fixes.) Because the
signature is stable, this grant **persists across future rebuilds**.

Adding the sidecar binary itself is a harmless extra, not required. If you ever
want to (e.g. debugging), reveal it and drag it onto the list:

```sh
open -R "/Applications/TenCalmDigits.app/Contents/MacOS/typeassist-input-macos"
```

> If a grant ever does get into a bad state, reset and re-grant:
> `tccutil reset Accessibility app.tencalmdigits && tccutil reset ListenEvent app.tencalmdigits`
