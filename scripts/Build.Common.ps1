Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-AStockRepositoryRoot {
    return (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
}

function Assert-AStockCleanWorktree {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    $status = @(& git -C $RepositoryRoot status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect the Git worktree.' }
    if ($status.Count -ne 0) {
        throw 'Immutable release evidence requires a completely clean worktree, including untracked files.'
    }
}

function Enable-AStockProtonAlloyRuntime {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # Proton 0.2.1 leaves CEF 147's windowed runtime style at its new Chrome
    # default, which creates a browser address bar around the desktop app.
    # Keep the pinned Proton release and apply the smallest auditable fix to
    # select Alloy for both headless and windowed child browsers.
    $protonRoot = Join-Path $RepositoryRoot 'desktop-moon\.mooncakes\moonbit-community\proton'
    $source = Join-Path $protonRoot 'internal\native\ffi\src\engine\cef_win\proton_engine_cef_win.c'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $text = Get-Content -LiteralPath $source -Raw
    $marker = 'browser_settings\.size = sizeof\(browser_settings\);\r?\n  window_info\.runtime_style = CEF_RUNTIME_STYLE_ALLOY;'
    if ([regex]::Matches($text, $marker).Count -ge 2) { return }
    $patch = Join-Path $RepositoryRoot 'patches\proton-0.2.1-cef147-alloy.patch'
    & git -C $RepositoryRoot apply --check --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) {
        throw 'Pinned Proton source does not match the audited CEF Alloy patch.'
    }
    & git -C $RepositoryRoot apply --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the Proton CEF Alloy patch.' }
}

function Enable-AStockProtonLiveResize {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # Keep the native parent black and clip its CEF child while Windows sends
    # interactive resize messages. This removes the white uncovered strip and
    # avoids redundant parent painting over the renderer HWND.
    $protonRoot = Join-Path $RepositoryRoot 'desktop-moon\.mooncakes\moonbit-community\proton'
    $source = Join-Path $protonRoot 'internal\native\ffi\src\engine\cef_win\proton_engine_cef_win.c'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $text = Get-Content -LiteralPath $source -Raw
    if ($text.Contains('wc.hbrBackground = (HBRUSH)GetStockObject(BLACK_BRUSH);')) { return }
    $patch = Join-Path $RepositoryRoot 'patches\proton-0.2.1-windows-live-resize.patch'
    & git -C $RepositoryRoot apply --check --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) {
        throw 'Pinned Proton source does not match the audited Windows live-resize patch.'
    }
    & git -C $RepositoryRoot apply --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the Proton Windows live-resize patch.' }
}

function Enable-AStockProtonGpuPolicy {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # Proton 0.2.1 disables Windows GPU acceleration unconditionally. Keep it
    # enabled for production and make software compositing an explicit,
    # auditable headless/fault-test mode.
    $protonRoot = Join-Path $RepositoryRoot 'desktop-moon\.mooncakes\moonbit-community\proton'
    $source = Join-Path $protonRoot 'internal\native\ffi\src\engine\cef_win\proton_engine_cef_win.c'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $text = Get-Content -LiteralPath $source -Raw
    if ($text.Contains('AStock production GPU policy: normal desktop windows keep hardware')) { return }
    $patch = Join-Path $RepositoryRoot 'patches\proton-0.2.1-windows-gpu-policy.patch'
    & git -C $RepositoryRoot apply --check --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) {
        throw 'Pinned Proton source does not match the audited Windows GPU policy patch.'
    }
    & git -C $RepositoryRoot apply --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the Proton Windows GPU policy patch.' }
}

function Enable-AStockProtonRendererRecovery {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # Proton 0.2.1 reports a terminated CEF renderer as a fatal bridge error.
    # The pinned Windows patch performs at most three reload recoveries per
    # minute before preserving Proton's fail-closed failure path.
    $protonRoot = Join-Path $RepositoryRoot 'desktop-moon\.mooncakes\moonbit-community\proton'
    $source = Join-Path $protonRoot 'internal\native\ffi\src\engine\cef_win\proton_engine_cef_win.c'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $text = Get-Content -LiteralPath $source -Raw
    if ($text.Contains('AStock production hardening: CEF does not recreate a crashed renderer')) { return }
    $patch = Join-Path $RepositoryRoot 'patches\proton-0.2.1-windows-renderer-recovery.patch'
    & git -C $RepositoryRoot apply --check --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) {
        throw 'Pinned Proton source does not match the audited Windows renderer recovery patch.'
    }
    & git -C $RepositoryRoot apply --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the Proton Windows renderer recovery patch.' }
}

function Enable-AStockProtonBundleWindowsPaths {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # MoonBit Path::relative currently returns the absolute candidate for
    # same-drive Windows paths. Proton Bundle then rejects an otherwise valid
    # D-drive renderer payload. Keep this containment-checked fix pinned to the
    # audited Proton Bundle 0.2.1 source.
    $bundleRoot = Join-Path $RepositoryRoot 'packaging-moon\.mooncakes\moonbit-community\proton_bundle'
    $source = Join-Path $bundleRoot 'support.mbt'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $text = Get-Content -LiteralPath $source -Raw
    if ($text.Contains('let comparable_root = if @path.sep')) { return }
    $patch = Join-Path $RepositoryRoot 'patches\proton-bundle-0.2.1-windows-relative-path.patch'
    & git -C $RepositoryRoot apply --check --unsafe-paths --directory='packaging-moon/.mooncakes/moonbit-community/proton_bundle' $patch
    if ($LASTEXITCODE -ne 0) {
        throw 'Pinned Proton Bundle source does not match the audited Windows path patch.'
    }
    & git -C $RepositoryRoot apply --unsafe-paths --directory='packaging-moon/.mooncakes/moonbit-community/proton_bundle' $patch
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the Proton Bundle Windows path patch.' }
}

function Enable-AStockProtonExplicitRemoteDebug {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    # Proton 0.2.1 treats a remote-debugging port as implicit permission to
    # enable CDP. AStock separates permission from port selection so production
    # shortcuts remain deny-by-default and release automation must explicitly
    # opt in through the desktop entry's ASTOCK_RELEASE_TEST_CDP gate.
    $protonRoot = Join-Path $RepositoryRoot 'desktop-moon\.mooncakes\moonbit-community\proton'
    $source = Join-Path $protonRoot 'facade_manifest.mbt'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return }
    $text = Get-Content -LiteralPath $source -Raw
    if ($text.Contains('AStock production hardening: choosing a port must never turn CDP on')) { return }
    $patch = Join-Path $RepositoryRoot 'patches\proton-0.2.1-explicit-remote-debug.patch'
    & git -C $RepositoryRoot apply --check --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) {
        throw 'Pinned Proton source does not match the audited explicit remote-debug patch.'
    }
    & git -C $RepositoryRoot apply --unsafe-paths --directory='desktop-moon/.mooncakes/moonbit-community/proton' $patch
    if ($LASTEXITCODE -ne 0) { throw 'Failed to apply the Proton explicit remote-debug patch.' }
}

function Initialize-AStockBuildEnvironment {
    param(
        [switch]$SkipSpaceCheck
    )

    $repoRoot = Get-AStockRepositoryRoot
    Enable-AStockProtonAlloyRuntime -RepositoryRoot $repoRoot
    Enable-AStockProtonGpuPolicy -RepositoryRoot $repoRoot
    Enable-AStockProtonLiveResize -RepositoryRoot $repoRoot
    Enable-AStockProtonRendererRecovery -RepositoryRoot $repoRoot
    Enable-AStockProtonBundleWindowsPaths -RepositoryRoot $repoRoot
    Enable-AStockProtonExplicitRemoteDebug -RepositoryRoot $repoRoot
    $buildRoot = if ([string]::IsNullOrWhiteSpace($env:ASTOCK_BUILD_ROOT)) {
        'D:\astock-build\astock-terminal'
    } else {
        $env:ASTOCK_BUILD_ROOT
    }

    # Windows PowerShell 5.1 runs on a .NET version without
    # Path.IsPathFullyQualified. IsPathRooted alone accepts drive-relative
    # values such as `D:folder`, so reject that form explicitly.
    $isDriveRelative = $buildRoot -match '^[A-Za-z]:[^\\/]'
    if (-not [System.IO.Path]::IsPathRooted($buildRoot) -or $isDriveRelative) {
        throw "ASTOCK_BUILD_ROOT must be an absolute path: $buildRoot"
    }

    $fullBuildRoot = [System.IO.Path]::GetFullPath($buildRoot)
    $rootPath = [System.IO.Path]::GetPathRoot($fullBuildRoot)
    if (-not (Test-Path -LiteralPath $rootPath -PathType Container)) {
        throw "Build volume is unavailable: $rootPath. Local builds never fall back to C:."
    }

    if (-not $SkipSpaceCheck) {
        $drive = [System.IO.DriveInfo]::new($rootPath)
        $minimum = 60GB
        if ($drive.AvailableFreeSpace -lt $minimum) {
            $availableGiB = [Math]::Round($drive.AvailableFreeSpace / 1GB, 2)
            throw "At least 60 GiB is required under $rootPath; available: $availableGiB GiB."
        }
    }

    $paths = [ordered]@{
        Root = $fullBuildRoot
        Cargo = Join-Path $fullBuildRoot 'cargo-target'
        MoonDesktop = Join-Path $fullBuildRoot 'moon-target\desktop'
        MoonAgent = Join-Path $fullBuildRoot 'moon-target\agent'
        MoonTools = Join-Path $fullBuildRoot 'moon-target\tools'
        ProtonRuntime = Join-Path $fullBuildRoot 'proton-runtime'
        NpmCache = Join-Path $fullBuildRoot 'npm-cache'
        ViteCache = Join-Path $fullBuildRoot 'vite-cache'
        RendererDist = Join-Path $fullBuildRoot 'renderer-dist'
        PackageStage = Join-Path $fullBuildRoot 'package-stage'
        Artifacts = Join-Path $fullBuildRoot 'artifacts'
        FormalCache = Join-Path $fullBuildRoot 'formal-cache'
        Temp = Join-Path $fullBuildRoot 'temp'
        Tools = Join-Path $fullBuildRoot 'tools'
    }

    foreach ($path in $paths.Values) {
        New-Item -ItemType Directory -Path $path -Force | Out-Null
    }

    $env:ASTOCK_BUILD_ROOT = $paths.Root
    $env:CARGO_TARGET_DIR = $paths.Cargo
    $env:PROTON_RUNTIME_STORE = $paths.ProtonRuntime
    $env:npm_config_cache = $paths.NpmCache
    $env:ASTOCK_VITE_CACHE = $paths.ViteCache
    $env:ASTOCK_RENDERER_DIST = $paths.RendererDist
    $env:ASTOCK_PACKAGE_STAGE = $paths.PackageStage
    $env:ASTOCK_ARTIFACTS_DIR = $paths.Artifacts
    $env:TEMP = $paths.Temp
    $env:TMP = $paths.Temp
    if (($env:PATH -split ';') -notcontains $paths.Tools) {
        $env:PATH = "$($paths.Tools);$env:PATH"
    }

    return [pscustomobject]@{
        RepositoryRoot = $repoRoot
        Paths = [pscustomobject]$paths
    }
}

function Get-ProtonCliPath {
    param([Parameter(Mandatory)]$Environment)

    $candidate = Join-Path $Environment.Paths.Tools 'proton_cli.exe'
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "Proton CLI is not bootstrapped. Run scripts/bootstrap.ps1 first."
    }
    return $candidate
}

function Enable-AStockCefRuntimePath {
    param([Parameter(Mandatory)]$Environment)

    $libcef = Get-ChildItem -LiteralPath $Environment.Paths.ProtonRuntime -Recurse -Filter 'libcef.dll' -File -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like '*\runtime\bin\libcef.dll' } |
        Select-Object -First 1
    if (-not $libcef) {
        throw "Pinned CEF runtime is unavailable under $($Environment.Paths.ProtonRuntime). Run scripts/bootstrap.ps1."
    }
    $runtimeBin = $libcef.DirectoryName
    if (($env:PATH -split ';') -notcontains $runtimeBin) {
        $env:PATH = "$runtimeBin;$env:PATH"
    }
    return $runtimeBin
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter()][string[]]$Arguments = @(),
        [Parameter()][string]$WorkingDirectory = (Get-Location).Path,
        [switch]$PreserveProxy
    )

    if (-not (Test-Path -LiteralPath $WorkingDirectory -PathType Container)) {
        throw "Working directory does not exist: $WorkingDirectory"
    }
    $proxyNames = @('ALL_PROXY', 'all_proxy', 'HTTP_PROXY', 'http_proxy', 'HTTPS_PROXY', 'https_proxy')
    $savedProxy = @{}
    $isMoonTool = ([System.IO.Path]::GetFileName($FilePath) -match '^(moon|moonx|proton_cli)(\.exe)?$')
    if ($isMoonTool -and -not $PreserveProxy) {
        foreach ($name in $proxyNames) {
            $value = [Environment]::GetEnvironmentVariable($name, 'Process')
            if ($value -match '^socks') {
                $savedProxy[$name] = $value
                [Environment]::SetEnvironmentVariable($name, $null, 'Process')
            }
        }
    }
    $exitCode = -1
    Push-Location $WorkingDirectory
    try {
        & $FilePath @Arguments
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
        foreach ($entry in $savedProxy.GetEnumerator()) {
            [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
        }
    }
    if ($exitCode -ne 0) {
        throw "Command failed with exit code ${exitCode}: $FilePath $($Arguments -join ' ')"
    }
}

function Set-AStockWorkerEnvironment {
    param(
        [Parameter(Mandatory)]$Environment,
        [switch]$Release
    )
    $mode = if ($Release) { 'release' } else { 'debug' }
    $engine = Join-Path $Environment.Paths.Cargo "$mode\astock-engine.exe"
    $agent = Join-Path $Environment.Paths.MoonAgent "native\$mode\build\agent_worker\agent_worker.exe"
    foreach ($worker in @($engine, $agent)) {
        if (-not (Test-Path -LiteralPath $worker -PathType Leaf)) {
            throw "Worker executable is missing: $worker"
        }
    }
    $env:ASTOCK_ENGINE_EXE = $engine
    $env:ASTOCK_AGENT_EXE = $agent
    $env:ASTOCK_ICON_PATH = Join-Path $Environment.RepositoryRoot 'assets\icons\icon.ico'
}

function New-AStockProtonConfig {
    param([Parameter(Mandatory)]$Environment)
    $links = @(
        @{ Path = Join-Path $Environment.RepositoryRoot 'ui\.astock-renderer-dist'; Target = $Environment.Paths.RendererDist },
        @{ Path = Join-Path $Environment.RepositoryRoot 'desktop-moon\.astock-artifacts'; Target = $Environment.Paths.Artifacts }
    )
    foreach ($link in $links) {
        if (Test-Path -LiteralPath $link.Path) {
            $item = Get-Item -LiteralPath $link.Path -Force
            if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
                throw "Build redirect exists but is not a junction: $($link.Path)"
            }
            continue
        }
        New-Item -ItemType Junction -Path $link.Path -Target $link.Target | Out-Null
    }
    return (Join-Path $Environment.RepositoryRoot 'desktop-moon\proton.project.json')
}
