#requires -Version 7.2

<#
.SYNOPSIS
Validates, packages and optionally publishes the immutable Agent Runtime prerelease.

.DESCRIPTION
This is the Windows fallback for environments where GitHub-hosted Actions cannot
start. It builds exactly the reviewed Runtime squash commit in an isolated Git
worktree, runs the same release gates as CI, creates MSI/NSIS installers plus
build metadata and SHA-256 sums, and publishes only when -Publish is supplied.

No source branch, working tree or existing tag is rewritten.

.EXAMPLE
pwsh -File .\scripts\release-agent-runtime.ps1

.EXAMPLE
pwsh -File .\scripts\release-agent-runtime.ps1 -Publish
#>

[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = "High")]
param(
    [switch]$Publish,
    [switch]$KeepWorktree
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$ExpectedRepository = "jacek4yang/astock-terminal"
$ReleaseTag = "v5.0.3-agent-runtime.1"
$ReleaseTargetSha = "159e3fbecb045b72e858aa631b58be9526941bbc"
$ApplicationVersion = "5.0.3"
$RustToolchain = "1.88.0"
$ReleaseNotesRelativePath = "docs/releases/v5.0.3-agent-runtime.1.md"
$MainRefspec = "+refs/heads/main:refs/remotes/origin/main"

function Require-Command {
    param([Parameter(Mandatory)][string]$Name)

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found in PATH."
    }
}

function Invoke-Native {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory)][string]$WorkingDirectory
    )

    Push-Location $WorkingDirectory
    try {
        Write-Host "`n> $FilePath $($ArgumentList -join ' ')" -ForegroundColor Cyan
        & $FilePath @ArgumentList
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            throw "Command failed with exit code $exitCode: $FilePath $($ArgumentList -join ' ')"
        }
    }
    finally {
        Pop-Location
    }
}

function Invoke-Capture {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory)][string]$WorkingDirectory
    )

    Push-Location $WorkingDirectory
    try {
        $output = & $FilePath @ArgumentList 2>&1
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            throw "Command failed with exit code $exitCode: $FilePath $($ArgumentList -join ' ')`n$($output -join [Environment]::NewLine)"
        }
        return (($output | ForEach-Object { $_.ToString() }) -join "`n").Trim()
    }
    finally {
        Pop-Location
    }
}

function Test-Native {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory)][string]$WorkingDirectory
    )

    Push-Location $WorkingDirectory
    try {
        & $FilePath @ArgumentList *> $null
        return $LASTEXITCODE -eq 0
    }
    finally {
        Pop-Location
    }
}

foreach ($command in @("git", "gh", "node", "npm", "rustup", "cargo")) {
    Require-Command $command
}

$InvocationDirectory = (Get-Location).Path
$RepositoryRoot = Invoke-Capture -FilePath "git" -ArgumentList @("rev-parse", "--show-toplevel") -WorkingDirectory $InvocationDirectory
$Repository = Invoke-Capture -FilePath "gh" -ArgumentList @("repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner") -WorkingDirectory $RepositoryRoot
if ($Repository -ne $ExpectedRepository) {
    throw "This release script is bound to $ExpectedRepository, but the current checkout is $Repository."
}

Invoke-Native -FilePath "gh" -ArgumentList @("auth", "status", "--hostname", "github.com") -WorkingDirectory $RepositoryRoot
$IsShallow = Invoke-Capture -FilePath "git" -ArgumentList @("rev-parse", "--is-shallow-repository") -WorkingDirectory $RepositoryRoot
if ($IsShallow -eq "true") {
    Invoke-Native -FilePath "git" -ArgumentList @("fetch", "origin", $MainRefspec, "--unshallow", "--tags", "--force") -WorkingDirectory $RepositoryRoot
}
else {
    Invoke-Native -FilePath "git" -ArgumentList @("fetch", "origin", $MainRefspec, "--tags", "--force") -WorkingDirectory $RepositoryRoot
}

if (-not (Test-Native -FilePath "git" -ArgumentList @("cat-file", "-e", "$ReleaseTargetSha`^{commit}") -WorkingDirectory $RepositoryRoot)) {
    Invoke-Native -FilePath "git" -ArgumentList @("fetch", "origin", $ReleaseTargetSha) -WorkingDirectory $RepositoryRoot
}
Invoke-Native -FilePath "git" -ArgumentList @("cat-file", "-e", "$ReleaseTargetSha`^{commit}") -WorkingDirectory $RepositoryRoot
Invoke-Native -FilePath "git" -ArgumentList @("merge-base", "--is-ancestor", $ReleaseTargetSha, "origin/main") -WorkingDirectory $RepositoryRoot

$ReleaseNotesPath = Join-Path $RepositoryRoot $ReleaseNotesRelativePath
if (-not (Test-Path -LiteralPath $ReleaseNotesPath -PathType Leaf)) {
    throw "Release notes are missing: $ReleaseNotesPath"
}
$ReleaseNotes = Get-Content -LiteralPath $ReleaseNotesPath -Raw
if (-not $ReleaseNotes.Contains($ReleaseTargetSha)) {
    throw "Release notes do not identify immutable source commit $ReleaseTargetSha."
}

$NodeVersion = Invoke-Capture -FilePath "node" -ArgumentList @("--version") -WorkingDirectory $RepositoryRoot
if ($NodeVersion -notmatch '^v20\.') {
    throw "Node.js 20.x is required; detected $NodeVersion."
}

Invoke-Native -FilePath "rustup" -ArgumentList @("toolchain", "install", $RustToolchain, "--profile", "minimal", "--component", "rustfmt", "clippy") -WorkingDirectory $RepositoryRoot

$TemporaryRoot = [IO.Path]::GetTempPath()
$WorktreePath = Join-Path $TemporaryRoot ("astock-agent-runtime-release-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N"))
$DistDirectory = Join-Path $RepositoryRoot ("dist/releases/{0}" -f $ReleaseTag)
$WorktreeAdded = $false

try {
    if (Test-Path -LiteralPath $DistDirectory) {
        Remove-Item -LiteralPath $DistDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $DistDirectory -Force | Out-Null

    Invoke-Native -FilePath "git" -ArgumentList @("worktree", "add", "--detach", $WorktreePath, $ReleaseTargetSha) -WorkingDirectory $RepositoryRoot
    $WorktreeAdded = $true

    $ActualSourceSha = Invoke-Capture -FilePath "git" -ArgumentList @("rev-parse", "HEAD") -WorkingDirectory $WorktreePath
    if ($ActualSourceSha -ne $ReleaseTargetSha) {
        throw "Detached worktree is at $ActualSourceSha instead of $ReleaseTargetSha."
    }

    $TauriConfig = Get-Content -LiteralPath (Join-Path $WorktreePath "src-tauri/tauri.conf.json") -Raw | ConvertFrom-Json
    $PackageJson = Get-Content -LiteralPath (Join-Path $WorktreePath "ui/package.json") -Raw | ConvertFrom-Json
    $PackageLock = Get-Content -LiteralPath (Join-Path $WorktreePath "ui/package-lock.json") -Raw | ConvertFrom-Json
    $CargoManifest = Get-Content -LiteralPath (Join-Path $WorktreePath "Cargo.toml") -Raw
    if ($TauriConfig.version -ne $ApplicationVersion) {
        throw "tauri.conf.json version is $($TauriConfig.version), expected $ApplicationVersion."
    }
    if ($PackageJson.version -ne $ApplicationVersion) {
        throw "ui/package.json version is $($PackageJson.version), expected $ApplicationVersion."
    }
    if ($PackageLock.version -ne $ApplicationVersion -or $PackageLock.packages."".version -ne $ApplicationVersion) {
        throw "ui/package-lock.json version does not match $ApplicationVersion."
    }
    if ($CargoManifest -notmatch '(?s)\[workspace\.package\].*?version\s*=\s*"5\.0\.3"') {
        throw "Cargo workspace version does not match $ApplicationVersion."
    }
    if (-not $ReleaseTag.StartsWith("v$ApplicationVersion-")) {
        throw "Release tag $ReleaseTag is not a prerelease of v$ApplicationVersion."
    }

    if (-not (Get-Command "cargo-audit" -ErrorAction SilentlyContinue)) {
        Invoke-Native -FilePath "cargo" -ArgumentList @("install", "cargo-audit", "--locked") -WorkingDirectory $WorktreePath
    }
    Invoke-Native -FilePath "cargo" -ArgumentList @("audit") -WorkingDirectory $WorktreePath

    Invoke-Native -FilePath "cargo" -ArgumentList @(
        "+$RustToolchain", "run", "--locked", "-p", "astock-evaluation", "--bin", "astock-eval", "--", "evaluate",
        "--dataset", "eval/datasets/p0-v1/dataset.json",
        "--thresholds", "eval/datasets/p0-v1/thresholds.json",
        "--baseline", "eval/baselines/p0-v1-test.json",
        "--json", "target/eval-reports/p0-v1-release.json",
        "--html", "target/eval-reports/p0-v1-release.html",
        "--split", "test", "--check"
    ) -WorkingDirectory $WorktreePath

    Invoke-Native -FilePath "npm" -ArgumentList @("ci", "--prefix", "ui") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "npm" -ArgumentList @("run", "build", "--prefix", "ui") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "cargo" -ArgumentList @("+$RustToolchain", "fmt", "--all", "--", "--check") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "cargo" -ArgumentList @("+$RustToolchain", "test", "--workspace", "--locked") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "cargo" -ArgumentList @("+$RustToolchain", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "cargo" -ArgumentList @("+$RustToolchain", "check", "-p", "astock-app", "--locked") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "npm" -ArgumentList @("test", "--prefix", "ui") -WorkingDirectory $WorktreePath

    $TauriCommand = Join-Path $WorktreePath "ui/node_modules/.bin/tauri.cmd"
    if (-not (Test-Path -LiteralPath $TauriCommand -PathType Leaf)) {
        throw "Tauri CLI was not installed at $TauriCommand."
    }
    Invoke-Native -FilePath $TauriCommand -ArgumentList @("build") -WorkingDirectory $WorktreePath
    Invoke-Native -FilePath "git" -ArgumentList @("diff", "--exit-code", "--", "Cargo.lock", "ui/package-lock.json") -WorkingDirectory $WorktreePath

    $BundleRoots = @(
        (Join-Path $WorktreePath "target/release/bundle"),
        (Join-Path $WorktreePath "src-tauri/target/release/bundle")
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Container }
    if ($BundleRoots.Count -eq 0) {
        throw "Tauri did not produce a bundle directory."
    }

    $BundleFiles = Get-ChildItem -Path $BundleRoots -Recurse -File
    $Msi = $BundleFiles | Where-Object Extension -EQ ".msi" | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
    $Nsis = $BundleFiles | Where-Object { $_.Extension -eq ".exe" -and $_.Name -match "setup" } | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
    if ($null -eq $Msi) {
        throw "No MSI installer was generated."
    }
    if ($null -eq $Nsis) {
        throw "No NSIS setup executable was generated."
    }

    $AssetVersion = $ReleaseTag.Substring(1)
    $MsiAsset = Join-Path $DistDirectory ("astock-terminal-{0}-windows-x86_64.msi" -f $AssetVersion)
    $NsisAsset = Join-Path $DistDirectory ("astock-terminal-{0}-windows-x86_64-setup.exe" -f $AssetVersion)
    Copy-Item -LiteralPath $Msi.FullName -Destination $MsiAsset -Force
    Copy-Item -LiteralPath $Nsis.FullName -Destination $NsisAsset -Force

    $MetadataPath = Join-Path $DistDirectory "BUILD-METADATA.txt"
    [IO.File]::WriteAllLines($MetadataPath, @(
        "release_tag=$ReleaseTag",
        "source_commit=$ReleaseTargetSha",
        "application_version=$ApplicationVersion",
        "platform=windows-x86_64"
    ), [Text.Encoding]::ASCII)

    $ChecksumPath = Join-Path $DistDirectory "SHA256SUMS"
    $ChecksumLines = foreach ($Asset in (Get-ChildItem -LiteralPath $DistDirectory -File | Sort-Object Name)) {
        if ($Asset.Name -eq "SHA256SUMS") {
            continue
        }
        $Hash = (Get-FileHash -LiteralPath $Asset.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "{0}  {1}" -f $Hash, $Asset.Name
    }
    [IO.File]::WriteAllLines($ChecksumPath, $ChecksumLines, [Text.Encoding]::ASCII)

    foreach ($Line in $ChecksumLines) {
        if ($Line -notmatch '^([0-9a-f]{64})  (.+)$') {
            throw "Malformed checksum line: $Line"
        }
        $ExpectedHash = $Matches[1]
        $AssetName = $Matches[2]
        $ActualHash = (Get-FileHash -LiteralPath (Join-Path $DistDirectory $AssetName) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($ActualHash -ne $ExpectedHash) {
            throw "Checksum mismatch for $AssetName."
        }
    }

    Write-Host "`nValidated release assets:" -ForegroundColor Green
    Get-ChildItem -LiteralPath $DistDirectory -File | Sort-Object Name | Format-Table Name, Length
    Get-Content -LiteralPath $ChecksumPath

    if (-not $Publish) {
        Write-Host "`nPackaging completed without publishing. Re-run with -Publish after reviewing $DistDirectory." -ForegroundColor Yellow
        return
    }

    if (-not $PSCmdlet.ShouldProcess("$Repository release $ReleaseTag", "Publish immutable prerelease from $ReleaseTargetSha")) {
        return
    }

    Invoke-Native -FilePath "git" -ArgumentList @("fetch", "origin", $MainRefspec, "--tags", "--force") -WorkingDirectory $RepositoryRoot
    if (Test-Native -FilePath "git" -ArgumentList @("rev-parse", "$ReleaseTag`^{commit}") -WorkingDirectory $RepositoryRoot) {
        $ExistingTagSha = Invoke-Capture -FilePath "git" -ArgumentList @("rev-parse", "$ReleaseTag`^{commit}") -WorkingDirectory $RepositoryRoot
        if ($ExistingTagSha -ne $ReleaseTargetSha) {
            throw "Existing tag $ReleaseTag points to $ExistingTagSha instead of $ReleaseTargetSha."
        }
    }

    $Assets = (Get-ChildItem -LiteralPath $DistDirectory -File | Sort-Object Name).FullName
    $ReleaseExists = Test-Native -FilePath "gh" -ArgumentList @("release", "view", $ReleaseTag, "--repo", $Repository) -WorkingDirectory $RepositoryRoot
    if ($ReleaseExists) {
        Invoke-Native -FilePath "gh" -ArgumentList (@("release", "upload", $ReleaseTag) + $Assets + @("--clobber", "--repo", $Repository)) -WorkingDirectory $RepositoryRoot
        Invoke-Native -FilePath "gh" -ArgumentList @(
            "release", "edit", $ReleaseTag,
            "--repo", $Repository,
            "--title", "AStock Terminal $ReleaseTag",
            "--notes-file", $ReleaseNotesPath,
            "--prerelease",
            "--draft=false"
        ) -WorkingDirectory $RepositoryRoot
    }
    else {
        Invoke-Native -FilePath "gh" -ArgumentList (@(
            "release", "create", $ReleaseTag
        ) + $Assets + @(
            "--repo", $Repository,
            "--target", $ReleaseTargetSha,
            "--title", "AStock Terminal $ReleaseTag",
            "--notes-file", $ReleaseNotesPath,
            "--prerelease",
            "--latest=false"
        )) -WorkingDirectory $RepositoryRoot
    }

    Invoke-Native -FilePath "git" -ArgumentList @("fetch", "origin", "--tags", "--force") -WorkingDirectory $RepositoryRoot
    $PublishedTagSha = Invoke-Capture -FilePath "git" -ArgumentList @("rev-parse", "$ReleaseTag`^{commit}") -WorkingDirectory $RepositoryRoot
    if ($PublishedTagSha -ne $ReleaseTargetSha) {
        throw "Published tag resolves to $PublishedTagSha instead of $ReleaseTargetSha."
    }

    $ReleaseJson = Invoke-Capture -FilePath "gh" -ArgumentList @(
        "release", "view", $ReleaseTag,
        "--repo", $Repository,
        "--json", "tagName,isDraft,isPrerelease,url,assets"
    ) -WorkingDirectory $RepositoryRoot | ConvertFrom-Json
    if ($ReleaseJson.tagName -ne $ReleaseTag -or $ReleaseJson.isDraft -or -not $ReleaseJson.isPrerelease) {
        throw "Published release metadata does not match the required non-draft prerelease contract."
    }

    $PublishedAssetNames = @($ReleaseJson.assets | ForEach-Object name)
    foreach ($ExpectedAsset in (Get-ChildItem -LiteralPath $DistDirectory -File).Name) {
        if ($PublishedAssetNames -notcontains $ExpectedAsset) {
            throw "Published release is missing asset $ExpectedAsset."
        }
    }

    Write-Host "`nPublished and verified: $($ReleaseJson.url)" -ForegroundColor Green
}
finally {
    if ($WorktreeAdded -and -not $KeepWorktree) {
        try {
            Invoke-Native -FilePath "git" -ArgumentList @("worktree", "remove", "--force", $WorktreePath) -WorkingDirectory $RepositoryRoot
        }
        catch {
            Write-Warning "Unable to remove Git worktree cleanly: $($_.Exception.Message)"
        }
    }
    elseif ($WorktreeAdded) {
        Write-Host "Kept diagnostic worktree: $WorktreePath" -ForegroundColor Yellow
    }

    if (-not $KeepWorktree -and (Test-Path -LiteralPath $WorktreePath)) {
        Remove-Item -LiteralPath $WorktreePath -Recurse -Force -ErrorAction SilentlyContinue
    }
}
