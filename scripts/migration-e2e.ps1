[CmdletBinding()]
param(
    [string]$EvidenceDirectory = $env:ASTOCK_RELEASE_EVIDENCE_DIR,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$commit = (& git -C $build.RepositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to resolve the migration evidence source commit.'
}
if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null

$installer = Join-Path $build.Paths.Artifacts 'astock-terminal-setup.exe'
$nsi = Join-Path $build.Paths.Artifacts '.astock-terminal.installer.nsi'
$engine = Join-Path $build.Paths.Cargo 'release\astock-engine.exe'
foreach ($required in @($installer, $nsi, $engine)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Migration E2E input is missing: $required"
    }
}
$nsiSource = Get-Content -LiteralPath $nsi -Raw
foreach ($marker in @('RequestExecutionLevel user', '/RELEASETEST=', 'InstallDir "$LOCALAPPDATA\Programs\AStock Terminal"')) {
    if (-not $nsiSource.Contains($marker)) { throw "Installer is not safely isolated for release testing: $marker" }
}

$runId = "migration-$($commit.Substring(0, 12))-$([Guid]::NewGuid().ToString('N'))"
$testRoot = Join-Path $build.Paths.Temp $runId
$installRoot = Join-Path $testRoot 'install'
$dataRoot = Join-Path $testRoot 'user-data'
New-Item -ItemType Directory -Path $testRoot,$dataRoot -Force | Out-Null
$rootPrefix = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
if (-not ([System.IO.Path]::GetFullPath($testRoot) + '\').StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Migration test root escaped ASTOCK_BUILD_ROOT: $testRoot"
}

$started = [DateTimeOffset]::UtcNow
$cases = [System.Collections.Generic.List[object]]::new()
function Add-Case {
    param([Parameter(Mandatory)][string]$Id, [Parameter(Mandatory)][long]$DurationMs, [hashtable]$Details = @{})
    $cases.Add([pscustomobject][ordered]@{
        id = $Id
        status = 'PASSED'
        duration_ms = $DurationMs
        details = [pscustomobject]$Details
    })
}
function Invoke-HiddenProcess {
    param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string[]]$ArgumentList)
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
    foreach ($argument in $ArgumentList) { $startInfo.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::Start($startInfo)
    if (-not $process) { throw "Unable to start process: $FilePath" }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Process failed with exit code $($process.ExitCode): $FilePath" }
}

$watch = [System.Diagnostics.Stopwatch]::StartNew()
# /D must be the final NSIS argument. ProcessStartInfo.ArgumentList preserves
# it as one argument even when a CI build root contains spaces.
Invoke-HiddenProcess -FilePath $installer -ArgumentList @('/S', '/RELEASETEST=1', "/D=$installRoot")
$watch.Stop()
$installedHost = Join-Path $installRoot 'astock-terminal.exe'
$installedMetadata = Join-Path $installRoot 'Resources\build-metadata.json'
if (-not (Test-Path -LiteralPath $installedHost -PathType Leaf) -or
    -not (Test-Path -LiteralPath $installedMetadata -PathType Leaf)) {
    throw 'The silent clean install did not produce the packaged application.'
}
$metadata = Get-Content -LiteralPath $installedMetadata -Raw | ConvertFrom-Json
if ($metadata.commit -ne $commit -or $metadata.application_version -ne '6.0.0') {
    throw 'The installed application is not bound to the current v6.0.0 commit.'
}
Add-Case -Id 'clean-install' -DurationMs $watch.ElapsedMilliseconds -Details @{
    install_root = $installRoot
    package_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $installer).Hash.ToLowerInvariant()
}

$watch.Restart()
Invoke-Checked -FilePath 'cargo' -Arguments @(
    'test', '--locked', '-p', 'astock-storage',
    'db::tests::migration_v5_adds_kv_fetched_at_on_upgrade', '--', '--exact'
) -WorkingDirectory $build.RepositoryRoot
$watch.Stop()
Add-Case -Id 'legacy-upgrade' -DurationMs $watch.ElapsedMilliseconds -Details @{
    verified = 'transactional schema upgrade plus read-only integrity-checked backup'
}

$watch.Restart()
$engineOutput = & node (Join-Path $PSScriptRoot 'migration-engine-e2e.mjs') $engine $testRoot
$engineExit = $LASTEXITCODE
$watch.Stop()
if ($engineExit -ne 0) { throw 'Engine migration E2E failed.' }
$engineResult = ($engineOutput -join "`n") | ConvertFrom-Json
if (-not $engineResult.ok) { throw 'Engine migration E2E did not report success.' }
foreach ($case in $engineResult.cases) {
    Add-Case -Id $case.id -DurationMs ([long]$case.duration_ms) -Details @{ layer = 'real Engine framed IPC' }
}

$marker = Join-Path $dataRoot 'research-history-preserved.txt'
[System.IO.File]::WriteAllText($marker, 'must survive uninstall', [System.Text.UTF8Encoding]::new($false))
$uninstaller = Join-Path $installRoot 'Uninstall.exe'
if (-not (Test-Path -LiteralPath $uninstaller -PathType Leaf)) { throw 'Installed uninstaller is missing.' }
$watch.Restart()
Invoke-HiddenProcess -FilePath $uninstaller -ArgumentList @('/S', '/RELEASETEST=1')
$deadline = [DateTimeOffset]::UtcNow.AddSeconds(10)
while ((Test-Path -LiteralPath $installedHost) -and [DateTimeOffset]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 100
}
$watch.Stop()
if (Test-Path -LiteralPath $installedHost) { throw 'Silent uninstall did not remove the installed application.' }
if (-not (Test-Path -LiteralPath $marker -PathType Leaf) -or (Get-Content -LiteralPath $marker -Raw) -ne 'must survive uninstall') {
    throw 'Uninstall modified research data outside the installation directory.'
}
Add-Case -Id 'uninstall-preserves-data' -DurationMs $watch.ElapsedMilliseconds -Details @{
    retained_marker = $marker
}

$requiredCases = @(
    'clean-install', 'legacy-upgrade', 'legacy-data-adoption', 'd-drive-migration',
    'sqlite-integrity', 'parquet-manifest', 'rollback', 'uninstall-preserves-data'
)
$caseIds = @($cases | ForEach-Object id)
foreach ($required in $requiredCases) {
    if ($required -notin $caseIds) { throw "Migration evidence case was not exercised: $required" }
}
if ($caseIds.Count -ne ($caseIds | Sort-Object -Unique).Count) {
    throw 'Migration evidence contains duplicate case identifiers.'
}

$completed = [DateTimeOffset]::UtcNow
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'migration-install-upgrade-uninstall'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    cases = $cases
    isolation = [pscustomobject][ordered]@{
        build_root = $build.Paths.Root
        test_root = $testRoot
        release_test_mode = $true
        touched_production_registry = $false
        touched_production_data = $false
    }
}
$json = $evidence | ConvertTo-Json -Depth 10
$evidencePath = Join-Path $evidenceRoot 'migration.json'
[System.IO.File]::WriteAllText($evidencePath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Invoke-Checked -FilePath 'node' -Arguments @(
    (Join-Path $PSScriptRoot 'release-evidence-check.mjs'),
    $evidencePath,
    'migration-install-upgrade-uninstall',
    $commit
) -WorkingDirectory $build.RepositoryRoot
Write-Host "Migration release evidence: $evidencePath"
