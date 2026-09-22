param([switch]$Debug)
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
$oldShimPath = $env:CODEX_IDENTITY_SHIM_PATH
try {
    $buildArgs = @('build', '--locked')
    $profile = 'debug'
    if (-not $Debug) { $buildArgs += '--release'; $profile = 'release' }
    & cargo @buildArgs -p codex-identity-shim
    if ($LASTEXITCODE -ne 0) { throw 'Identity shim build failed' }
    $env:CODEX_IDENTITY_SHIM_PATH = Join-Path $PSScriptRoot "target/$profile/codex_identity_shim.dll"
    & cargo @buildArgs -p codex-updater
    if ($LASTEXITCODE -ne 0) { throw 'Launcher build failed' }
} finally {
    $env:CODEX_IDENTITY_SHIM_PATH = $oldShimPath
    Pop-Location
}
