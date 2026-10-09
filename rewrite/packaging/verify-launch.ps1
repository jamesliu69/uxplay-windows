#Requires -Version 5.1
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string] $InstallerPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$path = (Resolve-Path -LiteralPath $InstallerPath).Path
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $null
$view = $null
$record = $null
$session = $null
try {
  $database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($path, 0))
  $view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database,
    @('SELECT Condition FROM LaunchCondition'))
  $null = $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)
  $condition = $null
  while ($true) {
    $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
    if (-not $record) { break }
    $candidate = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @(1))
    if ($candidate -match 'VersionNT64') { $condition = $candidate; break }
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($record)
    $record = $null
  }
  if (-not $condition) { throw 'Installer has no OS launch condition.' }
  $session = $installer.GetType().InvokeMember('OpenPackage', 'InvokeMethod', $null, $installer, @($path, 1))
  $cases = @(
    @{ Name='Windows 10 1809 with MSI compatibility version'; Bitness='603'; Build='17763'; Installed=''; Expected=1 },
    @{ Name='Windows 11 with MSI compatibility version'; Bitness='603'; Build='26300'; Installed=''; Expected=1 },
    @{ Name='Windows 10 below minimum build'; Bitness='603'; Build='17762'; Installed=''; Expected=0 },
    @{ Name='Windows 8.1'; Bitness='603'; Build='9600'; Installed=''; Expected=0 },
    @{ Name='32-bit Windows'; Bitness=''; Build='26300'; Installed=''; Expected=0 },
    @{ Name='Missing actual build'; Bitness='603'; Build=''; Installed=''; Expected=0 },
    @{ Name='Installed maintenance/uninstall'; Bitness=''; Build=''; Installed='1'; Expected=1 }
  )
  foreach ($case in $cases) {
    foreach ($entry in @(
      @('VersionNT64', $case.Bitness), @('WindowsBuild', '9600'),
      @('UXPLAY_WINDOWS_BUILD', $case.Build), @('Installed', $case.Installed)
    )) {
      $null = $session.GetType().InvokeMember('Property', 'SetProperty', $null, $session, $entry)
    }
    $result = $session.GetType().InvokeMember('EvaluateCondition', 'InvokeMethod', $null, $session, @($condition))
    if ($result -ne $case.Expected) {
      throw "$($case.Name): expected $($case.Expected), got $result. Condition: $condition"
    }
    Write-Host "PASS: $($case.Name)"
  }
  # AppSearch is read-only; execute the authored registry lookup itself.
  $null = $session.GetType().InvokeMember('Property', 'SetProperty', $null, $session, @('Installed', ''))
  $null = $session.GetType().InvokeMember('Property', 'SetProperty', $null, $session, @('VersionNT64', '603'))
  $status = $session.GetType().InvokeMember('DoAction', 'InvokeMethod', $null, $session, @('AppSearch'))
  if ($status -ne 1) { throw "AppSearch failed: $status" }
  $actual = $session.GetType().InvokeMember('Property', 'GetProperty', $null, $session, @('UXPLAY_WINDOWS_BUILD'))
  $nativeKey = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
    [Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
  try {
    $versionKey = $nativeKey.OpenSubKey('SOFTWARE\Microsoft\Windows NT\CurrentVersion')
    try { $expected = [string]$versionKey.GetValue('CurrentBuildNumber') }
    finally { if ($versionKey) { $versionKey.Dispose() } }
  } finally { $nativeKey.Dispose() }
  if ($actual -ne $expected) { throw "AppSearch build $actual differs from native registry $expected" }
  Write-Host "PASS: real 64-bit registry AppSearch build=$actual; 8 checks passed"
} finally {
  foreach ($object in @($record, $view, $database, $session, $installer)) {
    if ($null -ne $object -and [Runtime.InteropServices.Marshal]::IsComObject($object)) {
      [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($object)
    }
  }
}
