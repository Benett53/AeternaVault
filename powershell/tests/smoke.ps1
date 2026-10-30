# Exercises the PowerShell module against a built aeternavault-cli in demo
# mode (temporary folders, no system changes). Throws on the first failure.
#
#   powershell -File powershell/tests/smoke.ps1 -Cli target/debug/aeternavault-cli.exe
#   pwsh -File powershell/tests/smoke.ps1 -Cli target/debug/aeternavault-cli
param([Parameter(Mandatory)][string]$Cli)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0

function Assert([bool]$Condition, [string]$What) {
    if (-not $Condition) { throw "FAILED: $What" }
    Write-Host "ok  $What"
}

$root = Join-Path ([IO.Path]::GetTempPath()) ("av-smoke-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force (Join-Path $root 'profile'), (Join-Path $root 'Data/Taxes') | Out-Null
Set-Content -LiteralPath (Join-Path $root 'Data/letter.txt') -Value 'Dear archive,' -NoNewline
Set-Content -LiteralPath (Join-Path $root 'Data/Taxes/receipt.txt') -Value '42 EUR' -NoNewline

$env:AETERNAVAULT_CLI = (Resolve-Path -LiteralPath $Cli).ProviderPath
$env:AETERNAVAULT_HOME = Join-Path $root 'home'
$env:AETERNAVAULT_PROFILE_ROOT = Join-Path $root 'profile'
Remove-Item Env:AETERNAVAULT_PASSPHRASE -ErrorAction SilentlyContinue

Import-Module (Join-Path $PSScriptRoot '../AeternaVault/AeternaVault.psd1') -Force

try {
    $dest = Set-AVDestination (Join-Path $root 'dest with space')
    Assert ($dest.destination -like '*dest with space') 'Set-AVDestination handles spaces'

    Add-AVSource (Join-Path $root 'Data') -Name 'Data' | Out-Null
    Assert (@(Get-AVSource | Where-Object { $_.name -eq 'Data' }).Count -eq 1) 'Add-AVSource / Get-AVSource'

    $result = Start-AVBackup
    Assert ($result.files -eq 2) 'Start-AVBackup copies two files'

    $backups = @(Get-AVBackup)
    Assert ($backups.Count -eq 1) 'Get-AVBackup lists one backup'

    $files = @(Get-AVFile latest)
    Assert ($files.Count -eq 2) 'Get-AVFile lists two files'

    $target = Join-Path $root 'restored'
    Restore-AVBackup latest -To $target -Confirm:$false | Out-Null
    Assert ((Get-Content -LiteralPath (Join-Path $target 'Data/letter.txt') -Raw) -eq 'Dear archive,') 'Restore-AVBackup'

    New-AVJob -Every Week -Day Sunday -At 10:00 -Name 'Weekly' | Out-Null
    Set-AVJob Weekly -At 11:15 | Out-Null
    $jobs = @(Get-AVJob)
    Assert ($jobs.Count -eq 1) 'New-AVJob / Set-AVJob / Get-AVJob'
    Remove-AVJob Weekly -Confirm:$false | Out-Null
    Assert (@(Get-AVJob).Count -eq 0) 'Remove-AVJob'

    $retention = Set-AVRetention -Enabled $true -KeepLast 4
    Assert ($retention.enabled -eq $true) 'Set-AVRetention'

    Set-AVConfig advanced.hardlink_unchanged false | Out-Null
    Assert ((Get-AVConfig advanced.hardlink_unchanged) -eq $false) 'Set-AVConfig / Get-AVConfig'

    $failed = $false
    try { Get-AVFile 'no-such-backup' | Out-Null } catch { $failed = $true }
    Assert $failed 'errors become PowerShell errors'

    # Encryption: reading needs the passphrase even with a remembered key.
    # Umlauts and the euro sign must arrive as UTF-8, the same as when typed in.
    $plainPass = "Gr$([char]0xFC)$([char]0xDF)e aus K$([char]0xF6)ln f$([char]0xFC)r 5 $([char]0x20AC)"
    $pass = ConvertTo-SecureString $plainPass -AsPlainText -Force
    $setup = Initialize-AVEncryption -Passphrase $pass -Remember -Confirm:$false
    Assert ($setup.recovery_key.Length -gt 20) 'Initialize-AVEncryption returns the recovery key'
    Start-AVBackup | Out-Null
    $denied = $false
    try { Get-AVFile latest | Out-Null } catch { $denied = $true }
    Assert $denied 'encrypted files are not listed without the passphrase'
    Assert (@(Get-AVFile latest -Passphrase $pass).Count -eq 2) 'Get-AVFile -Passphrase'
    $env:AETERNAVAULT_PASSPHRASE = $plainPass
    try { Assert (@(Get-AVFile latest).Count -eq 2) 'passphrase with umlauts matches the environment variable' }
    finally { Remove-Item Env:AETERNAVAULT_PASSPHRASE }
    $recovery = ConvertTo-SecureString $setup.recovery_key -AsPlainText -Force
    Test-AVRecoveryKey -RecoveryKey $recovery | Out-Null
    Write-Host 'ok  Test-AVRecoveryKey'

    Assert ((Get-AVStatus).backups -ge 2) 'Get-AVStatus'
    Assert (@(Get-AVHistory).Count -ge 3) 'Get-AVHistory'
    $csv = Export-AVHistory (Join-Path $root 'activity.csv') -Format Csv
    Assert ((Get-Content -LiteralPath $csv.FullName -TotalCount 1) -eq 'time,severity,event,detail') 'Export-AVHistory'
    Rename-AVBackup latest 'Before update' | Out-Null
    Assert (@(Get-AVBackup | Where-Object { $_.name -eq 'Before update' }).Count -eq 1) 'Rename-AVBackup'
    Write-Host 'All module checks passed.'
}
finally {
    Remove-Module AeternaVault -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
