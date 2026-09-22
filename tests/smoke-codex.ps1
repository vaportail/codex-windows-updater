param([Parameter(Mandatory)][string]$AppDir)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$native = Join-Path $AppDir 'resources/native/windows-updater.node'
$backup = Join-Path $AppDir 'resources/native/windows-updater.bridge-test-original'
if (Test-Path -LiteralPath $backup) { throw 'Test backup exists; refusing overwrite' }
$profile = Join-Path $repo 'target/native-bridge-startup-profile'
$savedEnvironment = @{}
foreach ($name in @('CODEX_ELECTRON_USER_DATA_PATH', 'CODEX_HOME', 'CODEX_UPDATER_LAUNCHER', 'BRIDGE_TEST_REPLY')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$env:CODEX_ELECTRON_USER_DATA_PATH = $profile
$env:CODEX_HOME = Join-Path $repo 'target/native-bridge-startup-home'
$env:CODEX_UPDATER_LAUNCHER = Join-Path $repo 'target/bridge-test-helper.exe'
$env:BRIDGE_TEST_REPLY = '{"protocol":1,"available":false}'
$testProcess = $null
$originalHash = (Get-FileHash -LiteralPath $native -Algorithm SHA256).Hash
Rename-Item -LiteralPath $native -NewName 'windows-updater.bridge-test-original'
try {
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/codex_native_updater.dll') -Destination $native
    $stdout = Join-Path $repo 'target/updater-research/startup.stdout.log'
    $stderr = Join-Path $repo 'target/updater-research/startup.stderr.log'
    $testProcess = Start-Process -FilePath (Join-Path $AppDir 'ChatGPT.exe') -ArgumentList ('--user-data-dir="' + $profile + '"') -WorkingDirectory $AppDir -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
    Start-Sleep -Seconds 20
    $testProcess.Refresh()
    [pscustomobject]@{Pid=$testProcess.Id; Exited=$testProcess.HasExited; Window=$testProcess.MainWindowTitle}
    if ($testProcess.HasExited) { throw "Test Codex exited: $($testProcess.ExitCode); inspect $stderr" }
    Select-String -Path $stdout,$stderr -Pattern 'bootstrap_import_main_succeeded|startup.*failed|Failed to load native|package identity|windows_core_runtime_launch_selected' | Select-Object -First 10 | ForEach-Object { $_.Line }
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }
    if ($testProcess -and !$testProcess.HasExited) { Stop-Process -Id $testProcess.Id -Force; $testProcess.WaitForExit(5000) | Out-Null }
    Remove-Item -LiteralPath $native -Force -ErrorAction Stop
    Rename-Item -LiteralPath $backup -NewName 'windows-updater.node'
    if ((Get-FileHash -LiteralPath $native -Algorithm SHA256).Hash -ne $originalHash) { throw 'Restored addon hash mismatch' }
    Write-Output 'Original addon restored and SHA-256 verified.'
}
