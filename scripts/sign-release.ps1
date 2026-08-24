[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$CertificateThumbprint,
    [Parameter(Mandatory)][string]$TimestampUrl,
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$commit = (& git -C $build.RepositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to resolve the immutable source commit for signing.'
}
Assert-AStockCleanWorktree -RepositoryRoot $build.RepositoryRoot

function Resolve-ReleaseTool {
    param([Parameter(Mandatory)][string]$Name, [string[]]$Candidates = @())
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw "Required release tool is missing: $Name"
}

function Assert-InBuildRoot {
    param([Parameter(Mandatory)][string]$Path)
    $root = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
    $resolved = [System.IO.Path]::GetFullPath($Path)
    if (-not $resolved.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Signing target escapes ASTOCK_BUILD_ROOT: $resolved"
    }
    return $resolved
}

$normalizedThumbprint = $CertificateThumbprint.Replace(' ', '').ToUpperInvariant()
$certificate = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
    Where-Object Thumbprint -eq $normalizedThumbprint |
    Select-Object -First 1
if (-not $certificate -or -not $certificate.HasPrivateKey) {
    throw "Usable code-signing certificate not found in CurrentUser\My: $normalizedThumbprint"
}
if ($certificate.NotBefore -gt (Get-Date) -or $certificate.NotAfter -le (Get-Date)) {
    throw 'The selected code-signing certificate is outside its validity period.'
}
$timestampUri = $null
if (-not [System.Uri]::TryCreate($TimestampUrl, [System.UriKind]::Absolute, [ref]$timestampUri) -or
    $timestampUri.Scheme -notin @('http', 'https')) {
    throw 'ASTOCK_RFC3161_TIMESTAMP_URL must be an absolute HTTP(S) RFC3161 endpoint.'
}

$signtool = Resolve-ReleaseTool -Name 'signtool' -Candidates @(
    'D:\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe',
    'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe'
)
$makensis = Resolve-ReleaseTool -Name 'makensis'
$artifacts = Assert-InBuildRoot $build.Paths.Artifacts
$appDirectory = Assert-InBuildRoot (Join-Path $artifacts 'astock-terminal')
$zipPath = Assert-InBuildRoot (Join-Path $artifacts 'astock-terminal.zip')
$setupPath = Assert-InBuildRoot (Join-Path $artifacts 'astock-terminal-setup.exe')
$stagingSetup = Assert-InBuildRoot (Join-Path $artifacts '.astock-terminal.staging-setup.exe')
$nsiPath = Assert-InBuildRoot (Join-Path $artifacts '.astock-terminal.installer.nsi')
foreach ($required in @($appDirectory, $nsiPath)) {
    if (-not (Test-Path -LiteralPath $required)) { throw "Required package input is missing: $required" }
}

function Invoke-SignPe {
    param([Parameter(Mandatory)][string]$Path)
    & $signtool sign /sha1 $normalizedThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 /d 'AStock Terminal' /v $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool failed for $Path" }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode verification failed for ${Path}: $($signature.Status)"
    }
}

$started = [DateTimeOffset]::UtcNow
$peFiles = @(Get-ChildItem -LiteralPath $appDirectory -Recurse -File |
    Where-Object Extension -in @('.exe', '.dll'))
if ($peFiles.Count -eq 0) { throw 'The packaged application contains no PE files.' }
foreach ($file in $peFiles) {
    $existing = Get-AuthenticodeSignature -LiteralPath $file.FullName
    if ($existing.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        Invoke-SignPe -Path $file.FullName
    }
}
foreach ($file in $peFiles) {
    $verified = Get-AuthenticodeSignature -LiteralPath $file.FullName
    if ($verified.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Packaged PE is not Authenticode Valid: $($file.FullName)"
    }
}

# NSIS 3.08+ expands %1 to the generated uninstaller path. The wrapper is on
# D: and contains no private key material; signtool resolves the certificate
# by CurrentUser\My thumbprint. A non-zero signing result aborts makensis.
$signingStage = Assert-InBuildRoot (Join-Path $build.Paths.FormalCache "signing-$($commit.Substring(0, 12))")
New-Item -ItemType Directory -Path $signingStage -Force | Out-Null
$signWrapper = Assert-InBuildRoot (Join-Path $signingStage 'sign-pe.cmd')
$wrapperLines = @(
    '@echo off',
    ('"{0}" sign /sha1 {1} /fd SHA256 /tr "{2}" /td SHA256 /d "AStock Terminal" /v "%~1"' -f $signtool, $normalizedThumbprint, $TimestampUrl),
    'exit /b %ERRORLEVEL%'
)
[System.IO.File]::WriteAllLines($signWrapper, $wrapperLines, [System.Text.UTF8Encoding]::new($false))
$nsiSource = Get-Content -LiteralPath $nsiPath -Raw
if ($nsiSource -match '(?im)^\s*!uninstfinalize\b') {
    throw 'Generated NSIS source unexpectedly contains an uninstaller finalizer.'
}
$signedNsi = Assert-InBuildRoot (Join-Path $signingStage 'astock-terminal-signed.nsi')
$uninstallFinalize = "!uninstfinalize '`"$signWrapper`" `"%1`"' = 0"
[System.IO.File]::WriteAllText(
    $signedNsi,
    $uninstallFinalize + [Environment]::NewLine + $nsiSource,
    [System.Text.UTF8Encoding]::new($false)
)
if (Test-Path -LiteralPath $stagingSetup) { Remove-Item -LiteralPath $stagingSetup -Force }
& $makensis /V4 $signedNsi
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $stagingSetup -PathType Leaf)) {
    throw 'NSIS did not rebuild the installer with a signed uninstaller.'
}
Move-Item -LiteralPath $stagingSetup -Destination $setupPath -Force
Invoke-SignPe -Path $setupPath

# The portable archive must contain the signed application, not the unsigned
# candidate emitted by the packaging adapter.
if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
Compress-Archive -Path (Join-Path $appDirectory '*') -DestinationPath $zipPath -CompressionLevel Optimal
if (-not (Test-Path -LiteralPath $zipPath -PathType Leaf)) { throw 'Signed ZIP was not generated.' }

$hostPath = Join-Path $appDirectory 'astock-terminal.exe'
$enginePath = Join-Path $appDirectory 'Resources\workers\astock-engine.exe'
$agentPath = Join-Path $appDirectory 'Resources\workers\astock-agent.exe'
$cefHelperPath = Join-Path $appDirectory 'cef_process.exe'
$requiredArtifacts = [ordered]@{
    host = $hostPath
    engine = $enginePath
    agent = $agentPath
    'cef-helper' = $cefHelperPath
    nsis = $setupPath
}
$evidenceArtifacts = @()
foreach ($entry in $requiredArtifacts.GetEnumerator()) {
    $path = Assert-InBuildRoot $entry.Value
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Required release artifact is not Authenticode Valid: $path"
    }
    $evidenceArtifacts += [pscustomobject][ordered]@{
        kind = $entry.Key
        path = $path
        authenticode_status = 'Valid'
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    }
}

$manifestEntries = @($setupPath, $zipPath, $hostPath, $enginePath, $agentPath, $cefHelperPath)
$manifestPath = Assert-InBuildRoot (Join-Path $artifacts 'SHA256SUMS')
$manifestLines = foreach ($path in $manifestEntries) {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    "$hash  $([System.IO.Path]::GetRelativePath($artifacts, $path).Replace('\', '/'))"
}
[System.IO.File]::WriteAllLines($manifestPath, $manifestLines, [System.Text.UTF8Encoding]::new($false))

$completed = [DateTimeOffset]::UtcNow
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null
$evidencePath = Join-Path $evidenceRoot 'signed-artifacts.json'
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'authenticode-valid-all-pe'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    cases = @(
        [pscustomobject]@{ id = 'packaged-pe-verification'; status = 'PASSED'; duration_ms = [int]($completed - $started).TotalMilliseconds },
        [pscustomobject]@{ id = 'nsis-signed-uninstaller'; status = 'PASSED'; duration_ms = 0 },
        [pscustomobject]@{ id = 'signed-zip-rebuild'; status = 'PASSED'; duration_ms = 0 }
    )
    artifacts = $evidenceArtifacts
}
$evidence | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $evidencePath -Encoding utf8NoBOM

Write-Host "Signed artifact evidence: $evidencePath"
Write-Host "Signed artifact hashes:   $manifestPath"
