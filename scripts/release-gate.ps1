[CmdletBinding()]
param(
    [string]$CertificateThumbprint = $env:ASTOCK_SIGNING_CERT_THUMBPRINT,
    [string]$EvidenceDirectory = $env:ASTOCK_RELEASE_EVIDENCE_DIR,
    [string]$TimestampUrl = $env:ASTOCK_RFC3161_TIMESTAMP_URL
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment
$repository = $build.RepositoryRoot
$started = [DateTimeOffset]::UtcNow
$commit = (& git -C $repository rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($commit)) {
    throw 'Unable to resolve the immutable source commit.'
}

$timestamp = [DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$reportDirectory = Join-Path $build.Paths.Artifacts "release-gate\$timestamp-$($commit.Substring(0, 12))"
$logDirectory = Join-Path $reportDirectory 'logs'
New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$EvidenceDirectory = [System.IO.Path]::GetFullPath($EvidenceDirectory)

$script:results = [System.Collections.Generic.List[object]]::new()
$script:failed = 0
$script:gateStatuses = @{}

function Resolve-AStockTool {
    param(
        [Parameter(Mandatory)][string]$Name,
        [string[]]$Candidates = @()
    )
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw "Required release tool is missing: $Name"
}

function Invoke-ReleaseGateStep {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Category,
        [Parameter(Mandatory)][ValidateSet(
            'FORMALLY PROVED',
            'MODEL CHECKED',
            'PROPERTY TESTED',
            'INTEGRATION TESTED',
            'FAULT-INJECTION TESTED',
            'ASSUMED/TRUSTED BOUNDARY',
            'NOT VERIFIED'
        )][string]$Classification,
        [Parameter(Mandatory)][scriptblock]$Action,
        [string[]]$Requires = @()
    )
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $status = 'PASSED'
    $details = [System.Collections.Generic.List[string]]::new()
    $unmet = @($Requires | Where-Object {
        -not $script:gateStatuses.ContainsKey($_) -or $script:gateStatuses[$_] -ne 'PASSED'
    })
    if ($unmet.Count -gt 0) {
        $status = 'SKIPPED'
        $script:failed += 1
        $details.Add("Prerequisite gates did not pass: $($unmet -join ', ')")
    } else {
        try {
            & $Action 2>&1 | ForEach-Object { $details.Add($_.ToString()) }
        } catch {
            $status = 'FAILED'
            $script:failed += 1
            $details.Add($_.Exception.Message)
            if ($_.ScriptStackTrace) { $details.Add($_.ScriptStackTrace) }
        }
    }
    $watch.Stop()
    $script:gateStatuses[$Name] = $status
    $safeName = $Name -replace '[^A-Za-z0-9_.-]', '_'
    $logPath = Join-Path $logDirectory "$safeName.log"
    [System.IO.File]::WriteAllLines($logPath, $details, [System.Text.UTF8Encoding]::new($false))
    $script:results.Add([pscustomobject][ordered]@{
        name = $Name
        category = $Category
        classification = $Classification
        status = $status
        duration_ms = $watch.ElapsedMilliseconds
        log = $logPath.Substring($reportDirectory.Length + 1)
    })
    Write-Host ("[{0}] {1} ({2} ms)" -f $status, $Name, $watch.ElapsedMilliseconds)
}

function Assert-ReleaseEvidence {
    param([Parameter(Mandatory)][string]$FileName, [Parameter(Mandatory)][string]$Gate)
    $path = Join-Path $EvidenceDirectory $FileName
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required evidence is missing: $path"
    }
    Invoke-Checked -FilePath 'node' -Arguments @('scripts/release-evidence-check.mjs', $path, $Gate, $commit)
    Get-FileHash -Algorithm SHA256 -LiteralPath $path | Format-List
}

function Assert-SigningCertificate {
    if ([string]::IsNullOrWhiteSpace($CertificateThumbprint)) {
        throw 'ASTOCK_SIGNING_CERT_THUMBPRINT is required for a production release.'
    }
    $normalized = $CertificateThumbprint.Replace(' ', '').ToUpperInvariant()
    $certificate = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object Thumbprint -eq $normalized |
        Select-Object -First 1
    if (-not $certificate) { throw "Code-signing certificate not found in CurrentUser\My: $normalized" }
    if (-not $certificate.HasPrivateKey) { throw 'The selected code-signing certificate has no private key.' }
    if ($certificate.NotBefore -gt (Get-Date) -or $certificate.NotAfter -le (Get-Date)) {
        throw 'The selected code-signing certificate is outside its validity period.'
    }
    $certificate | Select-Object Subject, Thumbprint, NotBefore, NotAfter, HasPrivateKey | Format-List
}

Push-Location $repository
try {
    Invoke-ReleaseGateStep 'repository-immutable-main' 'source' 'INTEGRATION TESTED' {
        $branch = (& git branch --show-current).Trim()
        if ($LASTEXITCODE -ne 0) { throw 'git branch failed' }
        if ($branch -ne 'main') { throw "Production gate must run from main; current=$branch" }
        $dirty = @(& git status --porcelain=v1 --untracked-files=all)
        if ($LASTEXITCODE -ne 0) { throw 'git status failed' }
        if ($dirty.Count -ne 0) { throw "Working tree is not clean:`n$($dirty -join "`n")" }
        Invoke-Checked -FilePath 'git' -Arguments @('fetch', '--prune', 'origin')
        $aheadBehind = (& git rev-list --left-right --count 'HEAD...origin/main').Trim() -split '\s+'
        if ($LASTEXITCODE -ne 0 -or $aheadBehind.Count -ne 2 -or $aheadBehind[0] -ne '0' -or $aheadBehind[1] -ne '0') {
            throw "main is not identical to origin/main: $($aheadBehind -join '/')"
        }
        $existingTag = @(& git ls-remote --tags origin 'refs/tags/v6.0.0')
        if ($LASTEXITCODE -ne 0) { throw 'Unable to verify remote tag state.' }
        if ($existingTag.Count -ne 0) { throw 'Remote v6.0.0 already exists; immutable release tags are never moved.' }
        "commit=$commit"
    }

    Invoke-ReleaseGateStep 'version-contract' 'source' 'INTEGRATION TESTED' {
        Invoke-Checked -FilePath 'node' -Arguments @('scripts/release-version-check.mjs', '6.0.0')
        Invoke-Checked -FilePath 'node' -Arguments @('protocol/codegen.mjs', '--check')
        Invoke-Checked -FilePath 'node' -Arguments @('--test', 'scripts/release-evidence-check.test.mjs')
    }

    Invoke-ReleaseGateStep 'architecture-cutover' 'architecture' 'INTEGRATION TESTED' {
        Invoke-Checked -FilePath 'node' -Arguments @('scripts/capability-parity-check.mjs', '--release')
        Invoke-Checked -FilePath 'node' -Arguments @('scripts/release-architecture-check.mjs')
    }

    Invoke-ReleaseGateStep 'rust-format' 'rust' 'INTEGRATION TESTED' {
        Invoke-Checked -FilePath 'cargo' -Arguments @('fmt', '--all', '--', '--check')
    }
    Invoke-ReleaseGateStep 'rust-workspace-tests' 'rust' 'PROPERTY TESTED' {
        Invoke-Checked -FilePath 'cargo' -Arguments @('test', '--locked', '--workspace')
    }
    Invoke-ReleaseGateStep 'rust-clippy' 'rust' 'INTEGRATION TESTED' {
        Invoke-Checked -FilePath 'cargo' -Arguments @('clippy', '--locked', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings')
    }
    Invoke-ReleaseGateStep 'rustsec' 'security' 'INTEGRATION TESTED' {
        $database = Join-Path $build.Paths.FormalCache 'rustsec-advisory-db'
        if (-not (Test-Path -LiteralPath (Join-Path $database '.git'))) {
            throw "Pinned local RustSec database is missing: $database"
        }
        Invoke-Checked -FilePath 'git' -Arguments @('-C', $database, 'pull', '--ff-only')
        Invoke-Checked -FilePath 'cargo' -Arguments @('audit', '--db', $database, '--no-fetch', '--deny', 'warnings')
    }
    Invoke-ReleaseGateStep 'dependency-policy' 'security' 'INTEGRATION TESTED' {
        Invoke-Checked -FilePath 'cargo' -Arguments @('deny', 'check', 'advisories', 'bans', 'licenses', 'sources')
    }

    Invoke-ReleaseGateStep 'renderer-tests-and-build' 'renderer' 'INTEGRATION TESTED' {
        Invoke-Checked -FilePath 'npm' -Arguments @('--prefix', 'ui', 'ci')
        Invoke-Checked -FilePath 'npm' -Arguments @('--prefix', 'ui', 'test')
        Invoke-Checked -FilePath 'npm' -Arguments @('--prefix', 'ui', 'run', 'build')
    }

    Invoke-ReleaseGateStep 'moonbit-check-test' 'agent' 'PROPERTY TESTED' {
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @('fmt', '--check', '--target-dir', $build.Paths.MoonAgent)
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @('check', '--target-dir', $build.Paths.MoonAgent)
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @('test', '--target', 'native', '--target-dir', $build.Paths.MoonAgent)
    }

    Invoke-ReleaseGateStep 'desktop-worker-supervision' 'desktop' 'FAULT-INJECTION TESTED' {
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @(
            'build', '--target', 'native', '--release', '--target-dir', $build.Paths.MoonAgent, 'agent_worker'
        )
        $env:ASTOCK_SUPERVISION_TEST_WORKER = Join-Path $build.Paths.MoonAgent 'native\release\build\agent_worker\agent_worker.exe'
        if (-not (Test-Path -LiteralPath $env:ASTOCK_SUPERVISION_TEST_WORKER -PathType Leaf)) {
            throw "Agent supervision test Worker is missing: $env:ASTOCK_SUPERVISION_TEST_WORKER"
        }
        Enable-AStockCefRuntimePath -Environment $build | Out-Null
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'desktop-moon') -Arguments @(
            'test', '--target', 'native', '--no-parallelize', '--target-dir', $build.Paths.MoonDesktop, 'backend/host'
        )
    }

    Invoke-ReleaseGateStep 'moonbit-agent-proofs' 'formal' 'FORMALLY PROVED' {
        $proofTarget = Join-Path $build.Paths.Root 'moon-target\agent-prove-release-bootstrap'
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @('prove', 'agent_formal', '--target-dir', $proofTarget)
        $baseConfig = Join-Path $proofTarget 'verif\why3.conf'
        if (-not (Test-Path -LiteralPath $baseConfig -PathType Leaf)) { throw 'Why3 configuration was not generated.' }
        $configPrefix = (Get-Content -LiteralPath $baseConfig -Raw) -replace '(?s)\[strategy\].*$', ''
        $proved = [ordered]@{}
        foreach ($prover in @(
            [pscustomobject]@{ Name = 'Z3'; Version = '5.1.0'; Slug = 'z3' },
            [pscustomobject]@{ Name = 'CVC5'; Version = '1.3.4'; Slug = 'cvc5' }
        )) {
            # Run every obligation independently with each solver. A parallel
            # Why3 alternative may stop after the first solver succeeds and is
            # therefore not evidence that the second solver proved anything.
            $config = Join-Path $build.Paths.FormalCache "why3-$($prover.Slug)-only.conf"
            $strategy = @"
[strategy]
code = "start:
c $($prover.Name),$($prover.Version) 5 1000
"
desc = "AStock release proof with $($prover.Name)"
name = "AStock_$($prover.Name)"
shortcut = "4"
"@
            [System.IO.File]::WriteAllText($config, $configPrefix + $strategy, [System.Text.UTF8Encoding]::new($false))
            $solverTarget = Join-Path $build.Paths.Root "moon-target\agent-prove-$($prover.Slug)-release"
            Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @('prove', 'agent_formal', '--why3-config', $config, '--target-dir', $solverTarget)
            $proofJson = Join-Path $solverTarget 'verif\agent_formal\agent_formal.proof.json'
            if (-not (Test-Path -LiteralPath $proofJson -PathType Leaf)) {
                throw "$($prover.Name) did not generate a MoonBit proof result."
            }
            $proof = Get-Content -LiteralPath $proofJson -Raw | ConvertFrom-Json
            if ($proof.result -ne 'success' -or [int]$proof.summary.valid -le 0 -or [int]$proof.summary.invalid -ne 0) {
                throw "$($prover.Name) did not prove every MoonBit obligation."
            }
            $sessions = @(Get-ChildItem -Recurse -Filter why3session.xml (Join-Path $solverTarget 'verif\agent_formal'))
            $sessionText = ($sessions | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw }) -join "`n"
            $proverMatch = [regex]::Match($sessionText, "<prover id=`"([^`"]+)`" name=`"$($prover.Name)`"")
            if (-not $proverMatch.Success) { throw "$($prover.Name) is absent from its Why3 proof session." }
            $escapedId = [regex]::Escape($proverMatch.Groups[1].Value)
            $validCount = ([regex]::Matches($sessionText, "<proof prover=`"$escapedId`"><result status=`"valid`"")).Count
            if ($validCount -le 0) { throw "$($prover.Name) did not discharge an obligation in its Why3 session." }
            $proved[$prover.Name] = $validCount
        }
        "z3_valid=$($proved.Z3); cvc5_valid=$($proved.CVC5)"
    }

    Invoke-ReleaseGateStep 'tlc-agent-model' 'formal' 'MODEL CHECKED' {
        $java = Resolve-AStockTool -Name 'java' -Candidates @('D:\Applications\Scoop\apps\openjdk21\current\bin\java.exe')
        $tla = Join-Path $build.Paths.Tools 'tla2tools-1.8.0.jar'
        if (-not (Test-Path -LiteralPath $tla -PathType Leaf)) { throw "TLA+ tools are missing: $tla" }
        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $tla).Hash.ToLowerInvariant()
        if ($actualHash -ne 'eabd140a70f49eb9305a3bd3f3df944eddf87e5a90d329789085f8953a80533a') {
            throw "TLA+ tools digest mismatch: $actualHash"
        }
        $tlcStateDirectory = Join-Path $build.Paths.FormalCache "tlc-$($commit.Substring(0, 12))"
        New-Item -ItemType Directory -Path $tlcStateDirectory -Force | Out-Null
        Invoke-Checked -FilePath $java -Arguments @('-XX:+UseParallelGC', '-cp', $tla, 'tlc2.TLC', '-workers', 'auto', '-metadir', $tlcStateDirectory, '-config', 'formal\AgentLifecycle.cfg', 'formal\AgentLifecycle.tla')
    }

    # The user requires the non-invasive Codex in-app browser acceptance to
    # pass before any packaged desktop process is started.
    Invoke-ReleaseGateStep 'browser-cdp-evidence' 'renderer' 'INTEGRATION TESTED' {
        Assert-ReleaseEvidence -FileName 'browser-cdp.json' -Gate 'browser-cdp'
    }

    Invoke-ReleaseGateStep 'package-proton-cef' 'package' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence') -Action {
        & (Join-Path $PSScriptRoot 'package.ps1')
        if ($LASTEXITCODE -ne 0) { throw 'Proton packaging failed.' }
    }

    Invoke-ReleaseGateStep 'fault-injection-core' 'reliability' 'FAULT-INJECTION TESTED' {
        & (Join-Path $PSScriptRoot 'fault-injection-e2e.ps1') -EvidenceDirectory $EvidenceDirectory -SkipSpaceCheck
        if ($LASTEXITCODE -ne 0) { throw 'Core fault-injection execution failed.' }
        Assert-ReleaseEvidence -FileName 'fault-injection-core.json' -Gate 'fault-injection-core'
    }
    Invoke-ReleaseGateStep 'fault-injection-desktop-evidence' 'reliability' 'FAULT-INJECTION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','fault-injection-core') -Action {
        & (Join-Path $PSScriptRoot 'fault-injection-desktop.ps1') -EvidenceDirectory $EvidenceDirectory -SkipSpaceCheck
        if ($LASTEXITCODE -ne 0) { throw 'Desktop fault-injection execution failed.' }
        Assert-ReleaseEvidence -FileName 'fault-injection.json' -Gate 'fault-injection'
    }
    Invoke-ReleaseGateStep 'desktop-window-native-evidence' 'desktop' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action {
        & (Join-Path $PSScriptRoot 'desktop-window-e2e.ps1') -EvidenceDirectory $EvidenceDirectory -AllowInteractiveInput -SkipSpaceCheck
        if ($LASTEXITCODE -ne 0) { throw 'Native desktop window acceptance failed.' }
        Assert-ReleaseEvidence -FileName 'desktop-window-native.json' -Gate 'desktop-window-native'
    }
    Invoke-ReleaseGateStep 'desktop-e2e-evidence' 'desktop' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','desktop-window-native-evidence') -Action {
        Assert-ReleaseEvidence -FileName 'desktop-e2e.json' -Gate 'desktop-e2e-40'
    }
    Invoke-ReleaseGateStep 'migration-evidence' 'storage' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action {
        & (Join-Path $PSScriptRoot 'migration-e2e.ps1') -EvidenceDirectory $EvidenceDirectory -SkipSpaceCheck
        Assert-ReleaseEvidence -FileName 'migration.json' -Gate 'migration-install-upgrade-uninstall'
    }
    Invoke-ReleaseGateStep 'performance-evidence' 'performance' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action {
        & (Join-Path $PSScriptRoot 'performance-e2e.ps1') -EvidenceDirectory $EvidenceDirectory -SkipSpaceCheck
        if ($LASTEXITCODE -ne 0) { throw 'Packaged Proton/CEF performance execution failed.' }
        Assert-ReleaseEvidence -FileName 'performance.json' -Gate 'performance-budgets'
    }
    Invoke-ReleaseGateStep 'external-services-evidence' 'providers' 'ASSUMED/TRUSTED BOUNDARY' {
        Assert-ReleaseEvidence -FileName 'external-services.json' -Gate 'minimax-plus-joinquant-live'
    }
    Invoke-ReleaseGateStep 'credential-rotation-evidence' 'security' 'ASSUMED/TRUSTED BOUNDARY' {
        Assert-ReleaseEvidence -FileName 'credential-rotation.json' -Gate 'credential-rotation'
    }

    Invoke-ReleaseGateStep 'sbom' 'security' 'INTEGRATION TESTED' {
        $syft = Resolve-AStockTool -Name 'syft'
        $sbom = Join-Path $reportDirectory 'AStock-Terminal-v6.0.0.cdx.json'
        Invoke-Checked -FilePath $syft -Arguments @("dir:$repository", '-o', "cyclonedx-json=$sbom")
        if (-not (Test-Path -LiteralPath $sbom -PathType Leaf)) { throw 'Syft did not produce the CycloneDX SBOM.' }
    }

    $productionSigningPrerequisites = @(
        'repository-immutable-main',
        'version-contract',
        'architecture-cutover',
        'rust-format',
        'rust-workspace-tests',
        'rust-clippy',
        'rustsec',
        'dependency-policy',
        'renderer-tests-and-build',
        'moonbit-check-test',
        'desktop-worker-supervision',
        'moonbit-agent-proofs',
        'tlc-agent-model',
        'browser-cdp-evidence',
        'package-proton-cef',
        'fault-injection-core',
        'fault-injection-desktop-evidence',
        'desktop-window-native-evidence',
        'desktop-e2e-evidence',
        'migration-evidence',
        'performance-evidence',
        'external-services-evidence',
        'credential-rotation-evidence',
        'sbom'
    )
    Invoke-ReleaseGateStep 'authenticode' 'signing' 'ASSUMED/TRUSTED BOUNDARY' -Requires $productionSigningPrerequisites -Action {
        Assert-SigningCertificate
        if ([string]::IsNullOrWhiteSpace($TimestampUrl)) {
            throw 'ASTOCK_RFC3161_TIMESTAMP_URL is required for production signing.'
        }
        & (Join-Path $PSScriptRoot 'sign-release.ps1') `
            -CertificateThumbprint $CertificateThumbprint `
            -TimestampUrl $TimestampUrl `
            -EvidenceDirectory $EvidenceDirectory
        if ($LASTEXITCODE -ne 0) { throw 'Release signing pipeline failed.' }
        Assert-ReleaseEvidence -FileName 'signed-artifacts.json' -Gate 'authenticode-valid-all-pe'
    }
} finally {
    Pop-Location
}

$finished = [DateTimeOffset]::UtcNow
$summary = [pscustomobject][ordered]@{
    schema_version = 1
    application_version = '6.0.0'
    protocol_version = 1
    commit = $commit
    started_at_utc = $started.ToString('o')
    completed_at_utc = $finished.ToString('o')
    build_root = $build.Paths.Root
    evidence_directory = $EvidenceDirectory
    status = if ($script:failed -eq 0) { 'PASSED' } else { 'FAILED' }
    failed_gates = $script:failed
    github_actions = 'NOT VERIFIED — billing/spending restriction; release gates executed locally'
    results = $script:results
}

$jsonPath = Join-Path $reportDirectory 'verification-report.json'
$htmlPath = Join-Path $reportDirectory 'verification-report.html'
$summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $jsonPath -Encoding utf8NoBOM

$rows = foreach ($result in $script:results) {
    $color = switch ($result.status) {
        'PASSED' { '#2fb171' }
        'SKIPPED' { '#e4aa42' }
        default { '#e05260' }
    }
    "<tr><td>$([System.Net.WebUtility]::HtmlEncode($result.name))</td><td>$($result.category)</td><td>$($result.classification)</td><td style='color:$color'>$($result.status)</td><td>$($result.duration_ms)</td><td>$([System.Net.WebUtility]::HtmlEncode($result.log))</td></tr>"
}
$html = @"
<!doctype html><html><head><meta charset="utf-8"><title>AStock v6.0.0 verification</title>
<style>body{font:14px Segoe UI,Arial;background:#091523;color:#dce7f5;margin:32px}table{border-collapse:collapse;width:100%}th,td{border:1px solid #29405a;padding:8px;text-align:left}th{background:#11243a}code{color:#8ec5ff}</style></head>
<body><h1>AStock Terminal v6.0.0 local release gate</h1><p>Status: <b>$($summary.status)</b> · commit <code>$commit</code></p>
<p>$([System.Net.WebUtility]::HtmlEncode($summary.github_actions))</p>
<table><thead><tr><th>Gate</th><th>Category</th><th>Reliability class</th><th>Status</th><th>ms</th><th>Log</th></tr></thead><tbody>$($rows -join "`n")</tbody></table></body></html>
"@
[System.IO.File]::WriteAllText($htmlPath, $html, [System.Text.UTF8Encoding]::new($false))

$hashManifest = Join-Path $reportDirectory 'SHA256SUMS'
$hashLines = foreach ($file in Get-ChildItem -File $reportDirectory) {
    if ($file.Name -eq 'SHA256SUMS') { continue }
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
    "$hash  $($file.Name)"
}
[System.IO.File]::WriteAllLines($hashManifest, $hashLines, [System.Text.UTF8Encoding]::new($false))
Get-ChildItem -Recurse -File $reportDirectory | ForEach-Object { $_.IsReadOnly = $true }

Write-Host "Verification report: $jsonPath"
Write-Host "HTML report:         $htmlPath"
if ($script:failed -ne 0) { exit 1 }
