[CmdletBinding()]
param(
    [switch]$SkipRustWorkspace,
    [switch]$SkipSpaceCheck
)

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck

Push-Location $build.RepositoryRoot
try {
    Invoke-Checked -FilePath 'node' -Arguments @('protocol/codegen.mjs', '--check')
    Invoke-Checked -FilePath 'node' -Arguments @('scripts/capability-parity-check.mjs')
    Invoke-Checked -FilePath 'npm' -Arguments @('--prefix', 'ui', 'test')
    Invoke-Checked -FilePath 'npm' -Arguments @('--prefix', 'ui', 'run', 'build')
    Invoke-Checked -FilePath 'cargo' -Arguments @('test', '--locked', '-p', 'astock-protocol', '-p', 'astock-engine')
    if (-not $SkipRustWorkspace) {
        Invoke-Checked -FilePath 'cargo' -Arguments @('test', '--locked', '--workspace')
    }
    Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'app-moon') -Arguments @(
        'test', '--target', 'native', '--target-dir', $build.Paths.MoonAgent
    )
    Invoke-Checked -FilePath 'cargo' -Arguments @(
        'build', '--locked', '--release', '-p', 'astock-engine'
    )
    Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'app-moon') -Arguments @(
        'build', '--target', 'native', '--release', '--target-dir', $build.Paths.MoonAgent, 'agent_worker'
    )
    $env:ASTOCK_SUPERVISION_TEST_WORKER = Join-Path $build.Paths.MoonAgent 'native\release\build\agent_worker\agent_worker.exe'
    if (-not (Test-Path -LiteralPath $env:ASTOCK_SUPERVISION_TEST_WORKER -PathType Leaf)) {
        throw "Agent supervision test Worker is missing: $env:ASTOCK_SUPERVISION_TEST_WORKER"
    }
    Set-AStockWorkerEnvironment -Environment $build -Release
    $previousDataDir = $env:ASTOCK_DATA_DIR
    try {
        $env:ASTOCK_DATA_DIR = Join-Path $build.Paths.Temp "host-durability-$PID-$([Guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $env:ASTOCK_DATA_DIR -Force | Out-Null
        Enable-AStockCefRuntimePath -Environment $build | Out-Null
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -Arguments @(
            'test', '--target', 'native', '--no-parallelize', '--target-dir', $build.Paths.MoonDesktop, 'backend/host'
        )
        $env:ASTOCK_HOST_DURABILITY_TEST = '1'
        try {
            Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -Arguments @(
                'test', '--target', 'native', '--no-parallelize', '--filter', '*Host owns Agent durability*',
                '--target-dir', $build.Paths.MoonDesktop, 'backend/host/worker_supervision_wbtest.mbt'
            )
        } finally {
            Remove-Item Env:ASTOCK_HOST_DURABILITY_TEST -ErrorAction SilentlyContinue
        }
    } finally {
        if ($null -eq $previousDataDir) { Remove-Item Env:ASTOCK_DATA_DIR -ErrorAction SilentlyContinue }
        else { $env:ASTOCK_DATA_DIR = $previousDataDir }
    }
    Invoke-Checked -FilePath 'node' -Arguments @(
        'scripts/ipc-smoke.mjs', $env:ASTOCK_ENGINE_EXE, $env:ASTOCK_AGENT_EXE
    )
    Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -Arguments @(
        'check', '--target', 'native', '--target-dir', $build.Paths.MoonDesktop
    )
    Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'packaging-moon') -Arguments @(
        'check', '--target', 'native', '--target-dir', $build.Paths.MoonTools
    )
} finally {
    Pop-Location
}
