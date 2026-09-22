param([ValidateSet('debug', 'release')][string]$ProbeProfile = 'debug')
$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    # Deliberately stage only the EXE, so a sibling DLL cannot mask an embedding failure.
    $stage = Join-Path (Get-Location) "target/identity bundle $([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $stage | Out-Null
    $launcher = Join-Path $stage 'codex-launcher.exe'
    Copy-Item target/release/codex-launcher.exe $launcher
    $addon = Join-Path $stage 'windows-updater.node'
    Copy-Item "target/$ProbeProfile/codex_identity_probe_addon.dll" $addon
    $probe = Join-Path (Get-Location) "target/$ProbeProfile/identity-probe.exe"
    $manifest = Join-Path (Get-Location) 'compat/test-manifest.xml'
    $resultPath = Join-Path $stage 'result.txt'
    $selfTest = Start-Process -FilePath $launcher -ArgumentList '--self-test' -WindowStyle Hidden -PassThru -Wait
    if ($selfTest.ExitCode -ne 0) { throw "Bundled self-test returned $($selfTest.ExitCode)" }
    & $launcher --launch-with-identity --exe $probe --manifest $manifest -- $addon $resultPath 'bundled identity passed' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Bundled launch returned $LASTEXITCODE" }
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $resultPath) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
    if (-not (Test-Path -LiteralPath $resultPath)) { throw 'Bundled identity test timed out' }
    if ((Get-Content -LiteralPath $resultPath -Raw) -ne '["bundled identity passed"]') { throw 'Bundled probe output differs' }
    Write-Host 'PASS: embedded release DLL, cache materialization, release startup gate, scoped identity'
} finally { Pop-Location }
