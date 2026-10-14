param([string]$PackageDir = 'target/acceptance/windows-x64')
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$package = Join-Path $repo $PackageDir
$metadata = (& cargo metadata --locked --format-version 1 | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0) { throw 'Cannot locate GPUI shader sources' }
$gpui = @($metadata.packages | Where-Object { $_.name -eq 'gpui-pre-windows' })
if ($gpui.Count -ne 1) { throw 'Expected exactly one Windows GPUI dependency' }
$source = Join-Path (Split-Path $gpui[0].manifest_path -Parent) 'src'
$root = Join-Path ([IO.Path]::GetTempPath()) ('runner-portable-' + [guid]::NewGuid().ToString('N'))
$app = $null
$hidden = @()
$names = 'APPDATA', 'CODEX_HOME', 'RUNNER_MAX_SESSIONS', 'RUNNER_TEST_DAEMON_ENDPOINT', 'RUNNER_TEST_MCP_ENDPOINT'
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    Copy-Item -LiteralPath $package -Destination (Join-Path $root 'app') -Recurse
    $appDir = Join-Path $root 'app'
    foreach ($line in Get-Content -LiteralPath (Join-Path $appDir 'SHA256SUMS.txt')) {
        $parts = $line -split '  ', 2
        if ($parts.Count -ne 2 -or (Get-FileHash -LiteralPath (Join-Path $appDir $parts[1]) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $parts[0]) {
            throw "Package checksum mismatch: $line"
        }
    }
    # Reproduce the user's machine: the build-time HLSL files are unavailable.
    foreach ($file in 'shaders.hlsl', 'color_text_raster.hlsl') {
        $original = Join-Path $source $file
        $temporary = "$original.portable-test"
        Move-Item -LiteralPath $original -Destination $temporary
        $hidden += @{ Original = $original; Temporary = $temporary }
    }
    $token = [guid]::NewGuid().ToString('N')
    $env:APPDATA = Join-Path $root 'data'
    $env:CODEX_HOME = Join-Path $root 'codex'
    $env:RUNNER_MAX_SESSIONS = '3'
    $env:RUNNER_TEST_DAEMON_ENDPOINT = "\\.\pipe\runner-portable-daemon-$token"
    $env:RUNNER_TEST_MCP_ENDPOINT = "\\.\pipe\runner-portable-mcp-$token"
    $stdout = Join-Path $root 'stdout.log'
    $stderr = Join-Path $root 'stderr.log'
    $app = Start-Process -FilePath (Join-Path $appDir 'Runner.exe') -WorkingDirectory $appDir -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        Start-Sleep -Milliseconds 500
        $app.Refresh()
        if ($app.HasExited) { throw "Runner exited during portable startup: $(Get-Content -LiteralPath $stderr -Raw)" }
    } while ($app.MainWindowHandle -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $deadline)
    if ($app.MainWindowHandle -eq [IntPtr]::Zero) { throw 'Runner did not create a native window' }
    & (Join-Path $appDir 'runner-agent-cli.exe') daemon status
    if ($LASTEXITCODE -ne 0) { throw 'The packaged CLI cannot connect to the application daemon' }
    $database = Join-Path $env:APPDATA 'com.wycstudios.runner-dev/runner.db'
    if (-not (Test-Path -LiteralPath $database)) { throw 'Portable build did not use development data' }
    Write-Host 'PASS: relocated package checksums, native window, daemon protocol and development data; build-time shaders unavailable; no agents started'
} finally {
    if ($null -ne $app) {
        if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force }
        & (Join-Path $root 'app/runner-agent-cli.exe') daemon stop
    }
    foreach ($file in $hidden) { Move-Item -LiteralPath $file.Temporary -Destination $file.Original }
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
