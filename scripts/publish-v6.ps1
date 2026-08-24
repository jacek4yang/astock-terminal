[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [Parameter(Mandatory)][string]$VerificationReport,
    [Parameter(Mandatory)][string]$CertificateThumbprint,
    [switch]$ConfirmProductionRelease,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $ConfirmProductionRelease) {
    throw 'Production publication requires the explicit -ConfirmProductionRelease switch.'
}

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$repository = $build.RepositoryRoot
$tag = 'v6.0.0'
$version = '6.0.0'
$requiredActionsDisclosure = 'GitHub Actions: NOT VERIFIED — billing/spending restriction; release gates executed locally'

function Resolve-InBuildRoot {
    param([Parameter(Mandatory)][string]$Path)
    $root = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
    $resolved = [System.IO.Path]::GetFullPath($Path)
    if (-not $resolved.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Release input escapes ASTOCK_BUILD_ROOT: $resolved"
    }
    return $resolved
}

function Assert-Manifest {
    param([Parameter(Mandatory)][string]$Manifest, [Parameter(Mandatory)][string]$BaseDirectory)
    if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) { throw "Hash manifest is missing: $Manifest" }
    $baseRoot = [System.IO.Path]::GetFullPath($BaseDirectory).TrimEnd('\') + '\'
    $seenTargets = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $entryCount = 0
    foreach ($line in Get-Content -LiteralPath $Manifest) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        if ($line -notmatch '^([a-fA-F0-9]{64})\s{2}(.+)$') { throw "Invalid SHA256SUMS entry: $line" }
        $relativeTarget = $Matches[2].Replace('/', '\')
        if ([System.IO.Path]::IsPathRooted($relativeTarget)) { throw "Manifest target must be relative: $relativeTarget" }
        $candidate = [System.IO.Path]::GetFullPath((Join-Path $baseRoot $relativeTarget))
        if (-not $candidate.StartsWith($baseRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Manifest target escapes its base directory: $relativeTarget"
        }
        if (-not $seenTargets.Add($candidate)) { throw "Duplicate SHA256SUMS target: $relativeTarget" }
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw "Manifest target is missing: $candidate" }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $candidate).Hash
        if ($actual -ne $Matches[1]) { throw "Manifest hash mismatch: $candidate" }
        $entryCount++
    }
    if ($entryCount -eq 0) { throw "Hash manifest is empty: $Manifest" }
}

function Assert-Authenticode {
    param([Parameter(Mandatory)][string]$Path, [string]$Thumbprint)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Signed PE is missing: $Path" }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode is not Valid for ${Path}: $($signature.Status)"
    }
    if ($Thumbprint -and (-not $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ne $Thumbprint)) {
        throw "Authenticode signer does not match the selected release certificate: $Path"
    }
}

Push-Location $repository
try {
    Assert-AStockCleanWorktree -RepositoryRoot $repository
    $branch = (& git branch --show-current).Trim()
    if ($LASTEXITCODE -ne 0 -or $branch -ne 'main') { throw "Release publication requires main; current=$branch" }
    Invoke-Checked -FilePath 'git' -Arguments @('fetch', '--prune', 'origin')
    $commit = (& git rev-parse HEAD).Trim()
    $originCommit = (& git rev-parse origin/main).Trim()
    if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$' -or $commit -ne $originCommit) {
        throw 'main must be an immutable clean commit identical to origin/main.'
    }
    $localTagExists = @(& git tag --list $tag).Count -ne 0
    $remoteTagLines = @(& git ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}")
    if ($LASTEXITCODE -ne 0) { throw "Unable to inspect remote tag $tag." }
    $remoteTagExists = $remoteTagLines.Count -ne 0
    $remoteTagObject = $null
    $remoteTagCommit = $null
    if ($remoteTagExists) {
        foreach ($line in $remoteTagLines) {
            if ($line -notmatch '^([a-f0-9]{40})\s+(.+)$') { throw "Invalid remote tag record: $line" }
            if ($Matches[2] -eq "refs/tags/$tag") { $remoteTagObject = $Matches[1] }
            if ($Matches[2] -eq "refs/tags/$tag^{}") { $remoteTagCommit = $Matches[1] }
        }
        if (-not $remoteTagObject) { throw "Remote tag object is missing for $tag." }
        if (-not $remoteTagCommit) { $remoteTagCommit = $remoteTagObject }
        if ($remoteTagCommit -ne $commit) {
            throw "Existing immutable remote tag $tag does not point to the verified commit. It will not be moved or deleted."
        }
    }
    if ($remoteTagExists -and -not $localTagExists) {
        Invoke-Checked -FilePath 'git' -Arguments @('fetch', 'origin', "refs/tags/$tag`:refs/tags/$tag")
        $localTagExists = $true
    }
    if ($localTagExists) {
        Invoke-Checked -FilePath 'git' -Arguments @('tag', '-v', $tag)
        $existingTagCommit = (& git rev-list -n 1 $tag).Trim()
        if ($existingTagCommit -ne $commit) {
            throw "Existing immutable tag $tag does not point to the verified commit. It will not be moved or deleted."
        }
        if ($remoteTagExists) {
            $localTagObject = (& git rev-parse "refs/tags/$tag").Trim()
            if ($localTagObject -ne $remoteTagObject) {
                throw "Local and remote immutable tag objects differ for $tag. Publication is refused."
            }
        }
    }

    Invoke-Checked -FilePath 'gh' -Arguments @('auth', 'status')
    $repositoryInfo = (& gh repo view --json nameWithOwner,visibility,defaultBranchRef) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect the GitHub repository.' }
    if ($repositoryInfo.visibility -ne 'PRIVATE') { throw 'The v6 release script refuses to change or publish a non-private repository.' }
    if ($repositoryInfo.defaultBranchRef.name -ne 'main') { throw 'The GitHub default branch is not main.' }
    & gh release view $tag --json id *> $null
    if ($LASTEXITCODE -eq 0) { throw "GitHub Release already exists: $tag" }

    $reportPath = Resolve-InBuildRoot $VerificationReport
    if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) { throw "Verification report is missing: $reportPath" }
    if (-not (Get-Item -LiteralPath $reportPath).IsReadOnly) { throw 'Verification report must be immutable/read-only.' }
    $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
    if ($report.status -ne 'PASSED' -or [int]$report.failed_gates -ne 0 -or $report.commit -ne $commit) {
        throw 'Verification report is not a PASSED report for the exact main commit.'
    }
    if ($report.github_actions -ne 'NOT VERIFIED — billing/spending restriction; release gates executed locally') {
        throw 'Verification report is missing the required GitHub Actions disclosure.'
    }
    $failedResult = @($report.results | Where-Object status -ne 'PASSED')
    if ($failedResult.Count -ne 0) { throw 'Verification report contains a non-PASSED gate.' }
    $reportDirectory = Split-Path -Parent $reportPath
    Assert-Manifest -Manifest (Join-Path $reportDirectory 'SHA256SUMS') -BaseDirectory $reportDirectory

    $evidenceDirectory = Resolve-InBuildRoot $report.evidence_directory
    $signedEvidencePath = Join-Path $evidenceDirectory 'signed-artifacts.json'
    Invoke-Checked -FilePath 'node' -Arguments @(
        (Join-Path $PSScriptRoot 'release-evidence-check.mjs'),
        $signedEvidencePath,
        'authenticode-valid-all-pe',
        $commit
    )
    $normalizedThumbprint = $CertificateThumbprint.Replace(' ', '').ToUpperInvariant()
    $signedEvidence = Get-Content -LiteralPath $signedEvidencePath -Raw | ConvertFrom-Json
    foreach ($artifact in $signedEvidence.artifacts) {
        Assert-Authenticode -Path (Resolve-InBuildRoot $artifact.path) -Thumbprint $normalizedThumbprint
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $artifact.path).Hash.ToLowerInvariant()
        if ($actual -ne $artifact.sha256) { throw "Signed artifact changed after evidence capture: $($artifact.path)" }
    }

    $artifactRoot = Resolve-InBuildRoot $build.Paths.Artifacts
    $appDirectory = Resolve-InBuildRoot (Join-Path $artifactRoot 'astock-terminal')
    $setup = Join-Path $artifactRoot 'astock-terminal-setup.exe'
    $evidencePePaths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($artifact in $signedEvidence.pe_inventory) {
        $artifactPath = Resolve-InBuildRoot $artifact.path
        Assert-Authenticode -Path $artifactPath
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $artifactPath).Hash.ToLowerInvariant()
        if ($actual -ne $artifact.sha256) { throw "Packaged PE changed after evidence capture: $artifactPath" }
        if (-not $evidencePePaths.Add($artifactPath)) { throw "Duplicate packaged PE evidence path: $artifactPath" }
    }
    $currentPePaths = @(
        Get-ChildItem -LiteralPath $appDirectory -Recurse -File |
            Where-Object Extension -in @('.exe', '.dll') |
            ForEach-Object FullName
        $setup
    )
    if ($currentPePaths.Count -ne $evidencePePaths.Count) {
        throw 'The packaged PE inventory changed after Authenticode evidence capture.'
    }
    foreach ($path in $currentPePaths) {
        if (-not $evidencePePaths.Contains([System.IO.Path]::GetFullPath($path))) {
            throw "Unsigned or unrecorded packaged PE appeared after evidence capture: $path"
        }
    }
    Assert-Manifest -Manifest (Join-Path $artifactRoot 'SHA256SUMS') -BaseDirectory $artifactRoot
    $zip = Join-Path $artifactRoot 'astock-terminal.zip'
    Assert-Authenticode -Path $setup -Thumbprint $normalizedThumbprint
    if (-not (Test-Path -LiteralPath $zip -PathType Leaf)) { throw "Signed portable ZIP is missing: $zip" }

    $metadataPath = Join-Path $build.Paths.RendererDist 'build-metadata.json'
    $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
    if ($metadata.application_version -ne $version -or $metadata.commit -ne $commit -or $metadata.platform -ne 'windows-x64') {
        throw 'Build metadata is not bound to the verified release commit.'
    }
    $sbom = Join-Path $reportDirectory 'AStock-Terminal-v6.0.0.cdx.json'
    if (-not (Test-Path -LiteralPath $sbom -PathType Leaf)) { throw "CycloneDX SBOM is missing: $sbom" }

    $notes = Join-Path $repository 'docs\releases\v6.0.0.md'
    $migration = Join-Path $repository 'docs\releases\v6.0.0-migration.md'
    $notesText = Get-Content -LiteralPath $notes -Raw
    if (-not $notesText.Contains($requiredActionsDisclosure)) { throw 'Release Notes are missing the exact GitHub Actions disclosure.' }
    if (-not (Test-Path -LiteralPath $migration -PathType Leaf)) { throw 'Migration notes are missing.' }

    $publishStage = Resolve-InBuildRoot (Join-Path $build.Paths.PackageStage "publish-$($commit.Substring(0, 12))")
    New-Item -ItemType Directory -Path $publishStage -Force | Out-Null
    $assets = [ordered]@{
        'AStock-Terminal-v6.0.0-windows-x64.zip' = $zip
        'AStock-Terminal-v6.0.0-windows-x64-setup.exe' = $setup
        'AStock-Terminal-v6.0.0-build-metadata.json' = $metadataPath
        'AStock-Terminal-v6.0.0.cdx.json' = $sbom
        'AStock-Terminal-v6.0.0-verification.json' = $reportPath
        'AStock-Terminal-v6.0.0-verification.html' = (Join-Path $reportDirectory 'verification-report.html')
        'AStock-Terminal-v6.0.0-migration.md' = $migration
    }
    foreach ($entry in $assets.GetEnumerator()) {
        if (-not (Test-Path -LiteralPath $entry.Value -PathType Leaf)) { throw "Release asset is missing: $($entry.Value)" }
        Copy-Item -LiteralPath $entry.Value -Destination (Join-Path $publishStage $entry.Key) -Force
    }
    $assetManifest = Join-Path $publishStage 'AStock-Terminal-v6.0.0-SHA256SUMS.txt'
    $manifestLines = foreach ($file in Get-ChildItem -LiteralPath $publishStage -File | Sort-Object Name) {
        if ($file.FullName -eq $assetManifest) { continue }
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
        "$hash  $($file.Name)"
    }
    [System.IO.File]::WriteAllLines($assetManifest, $manifestLines, [System.Text.UTF8Encoding]::new($false))
    Assert-Manifest -Manifest $assetManifest -BaseDirectory $publishStage

    $tagAction = if ($localTagExists) { "reuse verified signed tag $tag" } else { "create signed tag $tag" }
    if (-not $PSCmdlet.ShouldProcess("$($repositoryInfo.nameWithOwner)@$commit", "$tagAction, push it if needed, and publish the Latest GitHub Release")) {
        return
    }
    if (-not $localTagExists) {
        Invoke-Checked -FilePath 'git' -Arguments @('tag', '-s', $tag, '-m', 'AStock Terminal v6.0.0')
        Invoke-Checked -FilePath 'git' -Arguments @('tag', '-v', $tag)
    }
    $tagCommit = (& git rev-list -n 1 $tag).Trim()
    if ($tagCommit -ne $commit) { throw 'Signed release tag does not point to the verified commit.' }
    if (-not $remoteTagExists) {
        Invoke-Checked -FilePath 'git' -Arguments @('push', 'origin', "refs/tags/$tag")
    }
    $releaseArguments = @('release', 'create', $tag, '--verify-tag', '--latest', '--title', 'AStock Terminal v6.0.0', '--notes-file', $notes)
    $releaseArguments += @(Get-ChildItem -LiteralPath $publishStage -File | Sort-Object Name | ForEach-Object FullName)
    Invoke-Checked -FilePath 'gh' -Arguments $releaseArguments
    Invoke-Checked -FilePath 'gh' -Arguments @('release', 'view', $tag, '--json', 'url,tagName,isDraft,isPrerelease')
} finally {
    Pop-Location
}
