@{
    RootModule           = 'AeternaVault.psm1'
    ModuleVersion        = '0.8.1'
    GUID                 = '5f0f3c52-8f7e-4b8e-9a51-6c2d7e1b4a90'
    Author               = 'AeternaVault contributors'
    Copyright            = 'MIT OR Apache-2.0'
    Description          = 'Manage AeternaVault backups, folders, jobs, retention and encryption from PowerShell. Wraps aeternavault-cli.'
    PowerShellVersion    = '5.1'
    CompatiblePSEditions = @('Desktop', 'Core')
    FunctionsToExport    = @(
        'Invoke-AVCli',
        'Get-AVStatus', 'Get-AVHistory', 'Get-AVPath',
        'Start-AVBackup', 'Get-AVBackup', 'Get-AVFile', 'Restore-AVBackup', 'Export-AVFile',
        'Test-AVBackup', 'Remove-AVBackup', 'Move-AVBackup', 'Invoke-AVPrune',
        'Get-AVSource', 'Add-AVSource', 'Remove-AVSource', 'Enable-AVSource', 'Disable-AVSource', 'Set-AVSourceItem',
        'Get-AVApp', 'Add-AVApp', 'Remove-AVApp',
        'Get-AVJob', 'New-AVJob', 'Set-AVJob', 'Remove-AVJob', 'Enable-AVJob', 'Disable-AVJob', 'Start-AVJob',
        'Get-AVRetention', 'Set-AVRetention', 'Get-AVDestination', 'Set-AVDestination',
        'Get-AVEncryption', 'Initialize-AVEncryption', 'Set-AVEncryption', 'Set-AVPassphrase',
        'Test-AVRecoveryKey', 'New-AVRecoveryKey', 'Set-AVRememberedKey',
        'Get-AVConfig', 'Set-AVConfig', 'Get-AVSystem', 'Set-AVSystem',
        'Rename-AVBackup', 'Export-AVHistory'
    )
    CmdletsToExport      = @()
    VariablesToExport    = @()
    AliasesToExport      = @()
    PrivateData          = @{
        PSData = @{
            Tags       = @('backup', 'restore', 'encryption', 'Windows', 'Linux')
            LicenseUri = 'https://github.com/Benett53/AeternaVault/blob/main/LICENSE-MIT'
            ProjectUri = 'https://github.com/Benett53/AeternaVault'
        }
    }
}
