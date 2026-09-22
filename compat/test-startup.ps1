param([switch]$Release)
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    $profile = 'debug'
    $buildArgs = @('build', '--locked', '-p', 'codex-identity-launcher', '-p', 'codex-identity-shim', '-p', 'codex-identity-probe-addon')
    if ($Release) { $profile = 'release'; $buildArgs += '--release' }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Probe build failed' }
    $testDir = Join-Path (Get-Location) "target/identity smoke $([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $testDir | Out-Null
    $addon = Join-Path $testDir 'windows-updater.node'
    Copy-Item "target/$profile/codex_identity_probe_addon.dll" $addon
    $output = Join-Path $testDir 'result.txt'
    & "./target/$profile/codex-identity-launcher.exe" "./target/$profile/identity-probe.exe" './compat/test-manifest.xml' $addon $output '' 'argument with spaces' 'quote"inside' 'C:\trailing slash\'
    if ($LASTEXITCODE -ne 0) { throw 'Startup injection failed' }
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $output) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
    if (-not (Test-Path -LiteralPath $output)) { throw 'Injected probe failed or timed out' }
    $result = Get-Content -LiteralPath $output -Raw
    if ($result -ne '["", "argument with spaces", "quote\"inside", "C:\\trailing slash\\"]') { throw "Argument forwarding failed: $result" }
    Write-Host 'PASS: startup gate, scoped Win32/WinRT identity, unaffected host, argument forwarding'

    # A DLL without our initialization export must fail before the app starts,
    # and must not leave a suspended process behind.
    $badDir = Join-Path $testDir 'missing-export'
    New-Item -ItemType Directory -Path $badDir | Out-Null
    Copy-Item "target/$profile/codex-identity-launcher.exe" $badDir
    Copy-Item "target/$profile/identity-probe.exe" $badDir
    Copy-Item "target/$profile/codex_identity_probe_addon.dll" (Join-Path $badDir 'codex_identity_shim.dll')
    $badOutput = Join-Path $badDir 'must-not-exist.txt'
    & (Join-Path $badDir 'codex-identity-launcher.exe') (Join-Path $badDir 'identity-probe.exe') './compat/test-manifest.xml' $addon $badOutput
    if ($LASTEXITCODE -eq 0 -or (Test-Path -LiteralPath $badOutput)) { throw 'Invalid shim was accepted' }
    if (Get-Process identity-probe -ErrorAction SilentlyContinue | Where-Object Path -EQ (Join-Path $badDir 'identity-probe.exe')) { throw 'Failed injection left a child behind' }
    Write-Host 'PASS: failed initialization terminates the held child'
    $global:LASTEXITCODE = 0
} finally { Pop-Location }
