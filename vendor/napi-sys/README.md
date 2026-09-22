# napi-sys 2.4.0

Vendored from the crates.io `napi-sys` 2.4.0 release of
https://github.com/napi-rs/napi-rs (MIT; see LICENSE).

Local change: `src/functions.rs::load_all` checks the executable for Node-API
exports, then uses the already loaded `chrome.dll` for the Owl runtime. Upstream
2.4.0 assumes exports are in the executable, which aborts ChatGPT.exe at addon
initialization. No new DLL is loaded and no process API is hooked.

The debug-only error variable also has an underscore prefix so release builds
pass this project's `-D warnings`. Other binding source is unmodified. Remove this patch when the upstream
loader supports this runtime. The standard Node and actual Codex startup tests
exercise both loader paths.
