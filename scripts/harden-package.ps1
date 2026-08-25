[CmdletBinding()]
param([switch]$SkipSpaceCheck)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$artifacts = $build.Paths.Artifacts
$sourcePath = Join-Path $artifacts '.astock-terminal.installer.nsi'
$stagingSetup = Join-Path $artifacts '.astock-terminal.staging-setup.exe'
$setupPath = Join-Path $artifacts 'astock-terminal-setup.exe'
if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
    throw "Generated NSIS source is missing: $sourcePath"
}

$source = Get-Content -LiteralPath $sourcePath -Raw
$required = @(
    'RequestExecutionLevel admin',
    'InstallDir "$PROGRAMFILES64\AStock Terminal"',
    'InstallDirRegKey HKLM',
    'WriteRegStr HKLM',
    'DeleteRegKey HKLM'
)
foreach ($marker in $required) {
    if (-not $source.Contains($marker)) {
        throw "Pinned Proton NSIS template drifted; expected marker is missing: $marker"
    }
}

# AStock is a per-user research workstation and never needs machine-wide
# privileges. Avoid UAC, keep unattended verification non-interactive, and
# ensure installation does not interfere with another user's profile.
$hardened = $source.Replace('RequestExecutionLevel admin', 'RequestExecutionLevel user')
$hardened = $hardened.Replace('InstallDir "$PROGRAMFILES64\AStock Terminal"', 'InstallDir "$LOCALAPPDATA\Programs\AStock Terminal"')
$hardened = $hardened.Replace('InstallDirRegKey HKLM', 'InstallDirRegKey HKCU')
$hardened = $hardened.Replace('WriteRegStr HKLM', 'WriteRegStr HKCU')
$hardened = $hardened.Replace('WriteRegDWORD HKLM', 'WriteRegDWORD HKCU')
$hardened = $hardened.Replace('DeleteRegKey HKLM', 'DeleteRegKey HKCU')
$testSupport = @'
!include "FileFunc.nsh"
!include "LogicLib.nsh"
Var ReleaseTest

Function .onInit
  ${GetParameters} $0
  ${GetOptions} $0 "/RELEASETEST=" $ReleaseTest
FunctionEnd

Function un.onInit
  ${GetParameters} $0
  ${GetOptions} $0 "/RELEASETEST=" $ReleaseTest
FunctionEnd
'@
$hardened = $hardened.Replace('!insertmacro MUI_LANGUAGE "English"', '!insertmacro MUI_LANGUAGE "English"' + [Environment]::NewLine + $testSupport)
$installKill = @'
  ; An update re-runs this installer while the application it is
  ; replacing is still running, so close it before overwriting.
  nsExec::Exec 'taskkill /F /IM "astock-terminal.exe"'
  Pop $0
  Sleep 500
'@
$installKillSafe = @'
  ${If} $ReleaseTest != "1"
    ; An update re-runs this installer while the application it is
    ; replacing is still running, so close it before overwriting.
    nsExec::Exec 'taskkill /F /IM "astock-terminal.exe"'
    Pop $0
    Sleep 500
  ${EndIf}
'@
$hardened = $hardened.Replace($installKill, $installKillSafe)
$installState = @'
  WriteRegStr HKCU "Software\com.astock.terminal" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\com.astock.terminal" "Version" "6.0.0"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "DisplayName" "AStock Terminal"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "DisplayVersion" "6.0.0"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "DisplayIcon" "$INSTDIR\astock-terminal.exe"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "NoRepair" 1

  CreateShortCut "$SMPROGRAMS\AStock Terminal.lnk" "$INSTDIR\astock-terminal.exe"
'@
$installStateSafe = @'
  ${If} $ReleaseTest != "1"
    WriteRegStr HKCU "Software\com.astock.terminal" "InstallDir" "$INSTDIR"
    WriteRegStr HKCU "Software\com.astock.terminal" "Version" "6.0.0"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "DisplayName" "AStock Terminal"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "DisplayVersion" "6.0.0"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "DisplayIcon" "$INSTDIR\astock-terminal.exe"
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal" "NoRepair" 1
    CreateShortCut "$SMPROGRAMS\AStock Terminal.lnk" "$INSTDIR\astock-terminal.exe"
  ${EndIf}
'@
$hardened = $hardened.Replace($installState, $installStateSafe)
$uninstallState = @'
  nsExec::Exec 'taskkill /F /IM "astock-terminal.exe"'
  Pop $0
  Delete "$SMPROGRAMS\AStock Terminal.lnk"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKCU "Software\com.astock.terminal"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal"
'@
$uninstallStateSafe = @'
  ${If} $ReleaseTest != "1"
    nsExec::Exec 'taskkill /F /IM "astock-terminal.exe"'
    Pop $0
    Delete "$SMPROGRAMS\AStock Terminal.lnk"
    DeleteRegKey HKCU "Software\com.astock.terminal"
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.astock.terminal"
  ${EndIf}
  RMDir /r "$INSTDIR"
'@
$hardened = $hardened.Replace($uninstallState, $uninstallStateSafe)
if ($hardened.Contains($installKill) -or $hardened.Contains($installState) -or $hardened.Contains($uninstallState)) {
    throw 'NSIS release-test isolation could not wrap every machine-affecting instruction.'
}
if ($hardened -match '(?im)^\s*RequestExecutionLevel\s+admin\b' -or
    $hardened -match '(?im)\bHKLM\b' -or
    $hardened.Contains('$PROGRAMFILES')) {
    throw 'NSIS hardening left a machine-wide installation directive behind.'
}
foreach ($marker in @('RequestExecutionLevel user', '/RELEASETEST=', '${If} $ReleaseTest != "1"', 'InstallDir "$LOCALAPPDATA\Programs\AStock Terminal"')) {
    if (-not $hardened.Contains($marker)) { throw "NSIS hardening failed to inject: $marker" }
}
[System.IO.File]::WriteAllText($sourcePath, $hardened, [System.Text.UTF8Encoding]::new($false))

$makensisCommand = Get-Command makensis -ErrorAction SilentlyContinue
if (-not $makensisCommand) { throw 'makensis is required to rebuild the hardened installer.' }
if (Test-Path -LiteralPath $stagingSetup) { Remove-Item -LiteralPath $stagingSetup -Force }
& $makensisCommand.Source /V3 $sourcePath
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $stagingSetup -PathType Leaf)) {
    throw 'NSIS did not emit the hardened per-user installer.'
}
Move-Item -LiteralPath $stagingSetup -Destination $setupPath -Force
Write-Host "Hardened per-user NSIS candidate: $setupPath"
