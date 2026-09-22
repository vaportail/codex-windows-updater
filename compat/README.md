# Package identity compatibility

The 26.915.4065.0 bootstrap calls `windows-updater.node.getCurrentPackageFamily()`
before importing the main app. That addon uses WinRT `Package.Current`, which
throws on an unpackaged process. The startup error is “The process has no package
identity.” Returning fake Win32 identity globally is insufficient: it also changes
Chromium's early packaged-app detection, and testing showed that it exited without
opening the app.

## Design

* `launcher` creates a new x64 process under `DEBUG_ONLY_THIS_PROCESS`, places a
  one-byte breakpoint at its PE entry point, restores the original instruction and
  instruction pointer, explicitly suspends the primary thread, and detaches.
* With loader initialization finished and app entry still held, it loads the shim
  and calls its initialization export on remote threads. Remote addresses use the
  target's module bases and export RVAs, including forwarded `LoadLibraryW` exports.
  It checks initialization before resuming and terminates its own child on failure.
  This does not promise interception of static DLL initializers that run before the
  executable entry point.
* `shim` intercepts native-addon loading. After loading `windows-updater.node` or
  `windows-account.node`, it redirects the addon's package-query IAT entries and
  `RoGetActivationFactory`. The latter provides a small immutable COM facade for
  `Windows.ApplicationModel.Package` / `IPackageStatics` / `IPackage` / `IPackageId`.
  Only identity metadata is implemented. Other activation classes use Windows;
  other processes' package queries use Windows too.
* `identity` reads the real MSIX manifest. Windows computes the full/family names
  and publisher ID; version and publisher strings are never baked into production.
  Win32 string lengths include the terminator; PACKAGE_ID lengths are byte counts,
  with every string pointer relocated into the caller's buffer.

No hook installation runs inside DllMain. The app executable and ASAR are untouched.
The implementation currently targets x64 Windows only. It does not inject into
renderers, existing processes, or unrelated applications. Addon code that switches
to delay imports, explicit GetProcAddress, or unsupported WinRT interfaces may need
additional work. Actual package registration/deployment and Store entitlements
remain outside the shim's scope.

## Build and test

From the repository root:

```powershell
./build.ps1
cargo test --workspace --all-targets
./compat/test-startup.ps1 -Release
./compat/test-bundle.ps1 -ProbeProfile release
```

The smoke test builds a native-addon fixture, verifies Win32 and WinRT properties
in that addon, confirms the host remains unpackaged, and checks argument quoting.
Its manifest is a test fixture, not a production identity source.

Standalone diagnostics:

```powershell
cargo build -p codex-identity-launcher -p codex-identity-shim
./target/debug/codex-identity-launcher.exe C:/path/ChatGPT.exe C:/path/AppxManifest.xml
```

The shim DLL must sit beside the standalone launcher. Optional
`CODEX_IDENTITY_TRACE=C:/path/trace.txt` records calls and makes the diagnostic
launcher observe the child's exit for up to ten seconds. Logs contain API names,
not credentials or manifest contents.

The bundled installer can also launch a specific older installation with a supplied
manifest, without a separate DLL next to the executable:

```powershell
./target/release/codex-launcher.exe --launch-with-identity --exe C:/path/ChatGPT.exe --manifest C:/path/AppxManifest.xml
```

Arguments after `--` are forwarded to the app. Use the original package manifest
for the version being launched, not the smoke-test fixture.

Confirmed manually against the supplied 26.915.4065.0 installation with a separate
test profile: the clean baseline displayed the missing-package-identity dialog;
the scoped WinRT shim opened the app, confirmed by the user. Authentication,
in-app Store updating, and future releases have not been validated.
