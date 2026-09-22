# Native updater bridge

Build with `./build.ps1 -Release`. The launcher embeds the
Rust Node-API addon and installs it as `resources/native/windows-updater.node`
during installation/update and before starting Codex. The original remains
`windows-updater.broken`. The installer checkbox "Use this launcher for in-app
Codex updates" is checked by default and saved as `native_updater_bridge` in
`updater.json`. Missing fields in older configs also default to `true`.

Set `native_updater_bridge` to `false` to keep the rename-only workaround. This
choice survives updates and installer elevation. Disabled configurations also
refuse bridge install requests and return no update from bridge checks, even if
an already-running app still has the addon loaded. Close Codex and launch again
to remove/install the sidecar after changing this setting.

Release builds require the sidecar payload; use the build script. Plain Cargo
debug builds support code checks, but need the build script to run with the
bridge enabled. CI and release workflows both use the standard build script.

The interface was inspected in Codex 26.915.4065.0 (`bootstrap-DK4EfNwt.js`).
No ASAR files, JavaScript, Windows package registration, or process APIs are patched.
The small vendored `napi-sys` patch resolves Node-API from Owl's already loaded
`chrome.dll`, while retaining executable exports for ordinary Node/Electron.

- Codex retains its online manifest gate, feature policy, and check interval.
  The bridge is not called if that gate says the app is current or disallows updates.
- `trySilentDownloadStoreUpdates` runs the exact launching executable with
  `--bridge-check` on a worker thread. It uses our existing version checker
  (currently the Direct catalog, independent of download fetcher) and returns
  availability through Codex's existing ready-to-install state.
- Codex's ready state means an update is **available**, not already downloaded.
  Downloading, signature verification, and extraction happen in our updater.
- `trySilentDownloadAndInstallStoreUpdates` starts that launcher with
  `--bridge-install`. It obtains elevation for system installs, closes only the
  app processes under that launcher's installation, then opens the update UI.
  Codex's normal pre-install preparation and shutdown callback remain in place.
  A handoff acknowledges process creation, not successful completion of an update.
- MSIX fallback initialization deliberately fails. Store deployment and optional
  registered-framework operations are unsupported; bundled runtime remains required.
- `getCurrentPackageFamily` returns the production manifest family
  `OpenAI.Codex_2p2nqsd0c76g0`. This also feeds Codex's sandbox configuration; it
  does not register a package or provide OS package identity. Other release
  channels and future native interfaces require compatibility testing.

The launcher passes its absolute path through `CODEX_UPDATER_LAUNCHER` (or its
installed copy for the install wizard's first Launch). Launching
ChatGPT.exe directly cannot check/install through the bridge. Our internal flags
do not get forwarded back to Codex. Checks neither save configuration nor close apps.
Installation follows the existing updater completion screen; use its Launch button
to reopen Codex. Canceling UAC or a failed download may leave Codex closed.

To disable, set `native_updater_bridge` to `false` and run the launcher: it recognizes the bridge's SHA-256
marker and removes only the matching bridge, preserving the original backup.
Close Codex before changing a loaded addon. An unrecognized `.node` alongside a
backup causes an error instead of overwriting either file.
Before reverting to an older release such as 0.1.4, close Codex and remove the
bridge `.node` and its `.launcher.sha256` marker; leave `.broken` intact. Older
launchers do not recognize bridge ownership markers.

## Verification

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p codex-updater --locked
cargo build -p codex-native-updater --locked
rustc tests/bridge-helper.rs -o target/bridge-test-helper.exe
node tests/native-updater.cjs target/debug/codex_native_updater.dll target/bridge-test-helper.exe
./build.ps1 -Release
node tests/launcher-bridge.cjs target/release/codex-launcher.exe
```

The Node test uses a mock launcher and does not terminate or update Codex. It
checks both availability states, errors, protocol rejection, disabled MSIX
fallback, and the install handoff. A real in-app update still needs manual testing
against a published newer Codex build; startup and the mocked handoff are tested.

`tests/codex-contract.cjs <app.asar> <sidecar.dll> <helper.exe>` additionally
executes the actual inspected Codex Store/wrapper updater classes with mocked
network and launcher operations. Its version-specific extraction fails explicitly
if the bundled updater contract changes.
