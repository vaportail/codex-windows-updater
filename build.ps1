param([switch]$Release)
$ErrorActionPreference = 'Stop'
$cargoArgs = @('build', '--locked')
$profile = 'debug'
if ($Release) { $cargoArgs += '--release'; $profile = 'release' }
$oldPayload = $env:CODEX_NATIVE_UPDATER_PATH
try {
    & cargo @cargoArgs -p codex-native-updater
    if ($LASTEXITCODE -ne 0) { throw 'Native updater build failed' }
    $env:CODEX_NATIVE_UPDATER_PATH = Join-Path $PSScriptRoot "target/$profile/codex_native_updater.dll"
    & cargo @cargoArgs -p codex-updater
    if ($LASTEXITCODE -ne 0) { throw 'Launcher build failed' }
} finally {
    $env:CODEX_NATIVE_UPDATER_PATH = $oldPayload
}
