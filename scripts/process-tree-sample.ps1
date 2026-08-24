[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, 2147483647)][int]$RootProcessId,
    [ValidateRange(1, 600)][int]$Samples = 1,
    [ValidateRange(100, 10000)][int]$IntervalMilliseconds = 1000
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-TargetSnapshot {
    $processes = @(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,KernelModeTime,UserModeTime,WorkingSetSize)
    $ids = [System.Collections.Generic.HashSet[uint32]]::new()
    [void]$ids.Add([uint32]$RootProcessId)
    do {
        $changed = $false
        foreach ($process in $processes) {
            if ($ids.Contains([uint32]$process.ParentProcessId) -and $ids.Add([uint32]$process.ProcessId)) {
                $changed = $true
            }
        }
    } while ($changed)
    $targets = @($processes | Where-Object { $ids.Contains([uint32]$_.ProcessId) })
    if (-not ($targets | Where-Object { [uint32]$_.ProcessId -eq [uint32]$RootProcessId })) {
        throw "Measured root process exited: $RootProcessId"
    }
    $cpu100ns = 0.0
    $workingSet = 0.0
    foreach ($process in $targets) {
        $cpu100ns += [double]$process.KernelModeTime + [double]$process.UserModeTime
        $workingSet += [double]$process.WorkingSetSize
    }
    return [pscustomobject]@{
        captured_at = [DateTimeOffset]::UtcNow
        cpu_100ns = $cpu100ns
        working_set_bytes = $workingSet
        process_count = $targets.Count
    }
}

$logicalProcessors = [Math]::Max(1, [System.Environment]::ProcessorCount)
$previous = Get-TargetSnapshot
$result = [System.Collections.Generic.List[object]]::new()
for ($index = 0; $index -lt $Samples; $index += 1) {
    Start-Sleep -Milliseconds $IntervalMilliseconds
    $current = Get-TargetSnapshot
    $elapsedSeconds = [Math]::Max(0.001, ($current.captured_at - $previous.captured_at).TotalSeconds)
    $cpuSeconds = [Math]::Max(0.0, ($current.cpu_100ns - $previous.cpu_100ns) / 10000000.0)
    $cpuPercent = ($cpuSeconds / $elapsedSeconds / $logicalProcessors) * 100.0
    $result.Add([pscustomobject][ordered]@{
        cpu_pct = [Math]::Round($cpuPercent, 6)
        working_set_bytes = [long]$current.working_set_bytes
        process_count = [int]$current.process_count
        interval_ms = [Math]::Round($elapsedSeconds * 1000.0, 3)
    })
    $previous = $current
}

[pscustomobject][ordered]@{
    root_process_id = $RootProcessId
    logical_processors = $logicalProcessors
    samples = $result
} | ConvertTo-Json -Depth 5 -Compress
