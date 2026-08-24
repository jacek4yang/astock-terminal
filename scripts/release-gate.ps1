[CmdletBinding()]
param(
    [string]$CertificateThumbprint = $env:ASTOCK_SIGNING_CERT_THUMBPRINT,
    [string]$EvidenceDirectory = $env:ASTOCK_RELEASE_EVIDENCE_DIR
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
        [Parameter(Mandatory)][scriptblock]$Action
    )
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $status = 'PASSED'
    $details = [System.Collections.Generic.List[string]]::new()
    try {
        & $Action 2>&1 | ForEach-Object { $details.Add($_.ToString()) }
    } catch {
        $status = 'FAILED'
        $script:failed += 1
        $details.Add($_.Exception.Message)
        if ($_.ScriptStackTrace) { $details.Add($_.ScriptStackTrace) }
    } finally {
        $watch.Stop()
    }
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
    $evidence = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    if ($evidence.gate -ne $Gate -or $evidence.status -ne 'PASSED' -or $evidence.commit -ne $commit) {
        throw "Evidence $FileName does not prove $Gate for commit $commit"
    }
    if (-not $evidence.completed_at_utc) {
        throw "Evidence $FileName has no completion time"
    }
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
        $parallelConfig = Join-Path $build.Paths.FormalCache 'why3-z3-cvc5.conf'
        $configPrefix = (Get-Content -LiteralPath $baseConfig -Raw) -replace '(?s)\[strategy\].*$', ''
        $strategy = @'
[strategy]
code = "start:
c Z3,5.1.0 5 1000 | CVC5,1.3.4 5 1000
"
desc = "AStock release proof with both pinned SMT solvers"
name = "AStock_Release"
shortcut = "4"
'@
        [System.IO.File]::WriteAllText($parallelConfig, $configPrefix + $strategy, [System.Text.UTF8Encoding]::new($false))
        $proofTarget = Join-Path $build.Paths.Root 'moon-target\agent-prove-release'
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @('prove', 'agent_formal', '--why3-config', $parallelConfig, '--target-dir', $proofTarget)
        $proofJson = Join-Path $proofTarget 'verif\agent_formal\agent_formal.proof.json'
        if (-not (Test-Path -LiteralPath $proofJson)) { throw 'MoonBit proof result was not generated.' }
        $proof = Get-Content -LiteralPath $proofJson -Raw | ConvertFrom-Json
        if ($proof.result -ne 'success' -or [int]$proof.summary.valid -le 0) { throw 'MoonBit proof obligations did not succeed.' }
        $sessions = @(Get-ChildItem -Recurse -Filter why3session.xml (Join-Path $proofTarget 'verif\agent_formal'))
        $sessionText = ($sessions | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw }) -join "`n"
        if ($sessionText -notmatch 'name="Z3"' -or $sessionText -notmatch 'name="CVC5"') {
            throw 'Why3 proof session did not exercise both Z3 and cvc5.'
        }
        $validByProver = @{ Z3 = 0; CVC5 = 0 }
        foreach ($session in $sessions) {
            $text = Get-Content -LiteralPath $session.FullName -Raw
            $proverNames = @{}
            foreach ($match in [regex]::Matches($text, '<prover id="([^"]+)" name="(Z3|CVC5)"')) {
                $proverNames[$match.Groups[1].Value] = $match.Groups[2].Value
            }
            foreach ($entry in $proverNames.GetEnumerator()) {
                $escapedId = [regex]::Escape($entry.Key)
                $count = ([regex]::Matches($text, "prover=`"$escapedId`"><result status=`"valid`"")).Count
                $validByProver[$entry.Value] += $count
            }
        }
        $validZ3 = $validByProver.Z3
        $validCvc5 = $validByProver.CVC5
        if ($validZ3 -le 0 -or $validCvc5 -le 0) { throw 'Both configured SMT solvers must discharge at least one obligation.' }
        "valid_goals=$($proof.summary.valid); z3_valid=$validZ3; cvc5_valid=$validCvc5"
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

    Invoke-ReleaseGateStep 'fault-injection-evidence' 'reliability' 'FAULT-INJECTION TESTED' {
        Assert-ReleaseEvidence -FileName 'fault-injection.json' -Gate 'fault-injection'
    }
    Invoke-ReleaseGateStep 'browser-cdp-evidence' 'renderer' 'INTEGRATION TESTED' {
        Assert-ReleaseEvidence -FileName 'browser-cdp.json' -Gate 'browser-cdp'
    }
    Invoke-ReleaseGateStep 'desktop-e2e-evidence' 'desktop' 'INTEGRATION TESTED' {
        Assert-ReleaseEvidence -FileName 'desktop-e2e.json' -Gate 'desktop-e2e-40'
    }
    Invoke-ReleaseGateStep 'migration-evidence' 'storage' 'INTEGRATION TESTED' {
        Assert-ReleaseEvidence -FileName 'migration.json' -Gate 'migration-install-upgrade-uninstall'
    }
    Invoke-ReleaseGateStep 'performance-evidence' 'performance' 'INTEGRATION TESTED' {
        Assert-ReleaseEvidence -FileName 'performance.json' -Gate 'performance-budgets'
    }
    Invoke-ReleaseGateStep 'external-services-evidence' 'providers' 'ASSUMED/TRUSTED BOUNDARY' {
        Assert-ReleaseEvidence -FileName 'external-services.json' -Gate 'minimax-plus-joinquant-live'
    }
    Invoke-ReleaseGateStep 'credential-rotation-evidence' 'security' 'ASSUMED/TRUSTED BOUNDARY' {
        Assert-ReleaseEvidence -FileName 'credential-rotation.json' -Gate 'credential-rotation'
    }

    Invoke-ReleaseGateStep 'package-proton-cef' 'package' 'INTEGRATION TESTED' {
        & (Join-Path $PSScriptRoot 'package.ps1')
        if ($LASTEXITCODE -ne 0) { throw 'Proton packaging failed.' }
    }

    Invoke-ReleaseGateStep 'sbom' 'security' 'INTEGRATION TESTED' {
        $syft = Resolve-AStockTool -Name 'syft'
        $sbom = Join-Path $reportDirectory 'AStock-Terminal-v6.0.0.cdx.json'
        Invoke-Checked -FilePath $syft -Arguments @("dir:$repository", '-o', "cyclonedx-json=$sbom")
        if (-not (Test-Path -LiteralPath $sbom -PathType Leaf)) { throw 'Syft did not produce the CycloneDX SBOM.' }
    }

    Invoke-ReleaseGateStep 'authenticode' 'signing' 'ASSUMED/TRUSTED BOUNDARY' {
        Assert-SigningCertificate
        Resolve-AStockTool -Name 'signtool' -Candidates @(
            'D:\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe',
            'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe'
        ) | Out-Null
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
    $color = if ($result.status -eq 'PASSED') { '#2fb171' } else { '#e05260' }
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
