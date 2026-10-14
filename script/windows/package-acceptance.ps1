param(
    [string]$SourceDir = 'target/ci',
    [string]$OutputDir = 'target/acceptance/windows-x64'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$source = Join-Path $repo $SourceDir
$output = Join-Path $repo $OutputDir
foreach ($binary in 'Runner.exe', 'runner-agent-cli.exe') {
    if (-not (Test-Path -LiteralPath (Join-Path $source $binary))) {
        throw "Missing acceptance binary: $binary"
    }
}
# Reuse the pinned checksum and Microsoft-signature verification used by make.cmd.
& (Join-Path $PSScriptRoot 'conpty.ps1') -OutputDir $source
New-Item -ItemType Directory -Force -Path $output | Out-Null
foreach ($binary in 'Runner.exe', 'runner-agent-cli.exe', 'conpty.dll', 'OpenConsole.exe') {
    Copy-Item -LiteralPath (Join-Path $source $binary) -Destination (Join-Path $output $binary) -Force
}
foreach ($notice in @{
    'LICENSE' = 'LICENSE.txt'
    'crates/runner-app/LICENSE.conpty' = 'LICENSE.conpty.txt'
    'crates/runner-app/LICENSE.xterm' = 'LICENSE.xterm.txt'
}.GetEnumerator()) {
    Copy-Item -LiteralPath (Join-Path $repo $notice.Key) -Destination (Join-Path $output $notice.Value) -Force
}
@'
@echo off
setlocal
set "CODEX_HOME=%LOCALAPPDATA%\RunnerForkTest\codex"
set "RUNNER_MAX_SESSIONS=3"
if not exist "%CODEX_HOME%" mkdir "%CODEX_HOME%"
cd /d "%~dp0"
"%~dp0Runner.exe"
endlocal
'@ | Set-Content -LiteralPath (Join-Path $output 'Start-Runner-Test.cmd') -Encoding ascii
$sha = (& git -C $repo rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify acceptance commit' }
@"
Runner Windows x64 development acceptance build
Commit: $sha
Profile: ci (development data; not an official signed release)

Keep all files together. Start Start-Runner-Test.cmd on Windows 11 Intel/AMD x64.
The launcher sets CODEX_HOME to %LOCALAPPDATA%\RunnerForkTest\codex and limits
Runner to 3 simultaneous sessions. Settings are local to the launched process.
Runner uses its separate development database (%APPDATA%\com.wycstudios.runner-dev).
Run runner-agent-cli.exe --help for CLI commands. Use a separate CODEX_HOME
and sign in there yourself if native agent tests are authorized. Credentials
and production user configuration are not included in this artifact.

See docs/tests/windows-orchestration-acceptance.md in the matching commit for
mock/CI evidence and the separate authorized native-agent acceptance checklist.
No upstream merge, publication or official release is implied by this build.
"@ | Set-Content -LiteralPath (Join-Path $output 'README.txt') -Encoding utf8
$hashes = Get-ChildItem -LiteralPath $output -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
}
$hashes | Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS.txt') -Encoding ascii
foreach ($binary in 'Runner.exe', 'runner-agent-cli.exe', 'conpty.dll', 'OpenConsole.exe') {
    if ((Get-Item -LiteralPath (Join-Path $output $binary)).Length -eq 0) { throw "Empty acceptance binary: $binary" }
}
Write-Host "Acceptance build prepared for commit $sha"
