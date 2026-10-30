# PowerShell commands for AeternaVault. Every command calls `aeternavault-cli`
# with --json and returns objects; errors become terminating PowerShell errors.
# Works in Windows PowerShell 5.1 and PowerShell 7 (Windows and Linux).

Set-StrictMode -Version 3.0

function Find-AVCli {
    if ($env:AETERNAVAULT_CLI -and (Test-Path -LiteralPath $env:AETERNAVAULT_CLI)) {
        return $env:AETERNAVAULT_CLI
    }
    $command = Get-Command -Name 'aeternavault-cli' -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($command) { return $command.Source }
    foreach ($dir in @($env:ProgramFiles, $env:LOCALAPPDATA)) {
        if (-not $dir) { continue }
        foreach ($sub in @('AeternaVault', 'Programs\AeternaVault')) {
            $candidate = Join-Path $dir (Join-Path $sub 'aeternavault-cli.exe')
            if (Test-Path -LiteralPath $candidate) { return $candidate }
        }
    }
    throw "aeternavault-cli was not found. Install AeternaVault, add it to PATH, or set `$env:AETERNAVAULT_CLI."
}

function ConvertTo-AVPlainText([securestring]$Secure) {
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Secure)
    try { [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}

function Format-AVArgument([string]$Value) {
    if ($Value -eq '') { return '""' }
    if ($Value -notmatch '[\s"]') { return $Value }
    # Windows command-line quoting: backslashes before a quote are doubled.
    $escaped = [regex]::Replace($Value, '(\\*)"', '$1$1\"')
    $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
    return '"' + $escaped + '"'
}

<#
.SYNOPSIS
Runs aeternavault-cli with the given arguments and returns the parsed JSON.
.DESCRIPTION
All other commands of this module use it. Secrets are passed on standard
input (--passphrase-stdin), never on the command line. Exit code 1 throws,
exit code 2 (completed with problems, or not confirmed) writes a warning.
#>
function Invoke-AVCli {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string[]]$Arguments,
        [securestring[]]$Secret,
        [switch]$Raw
    )
    $all = @('--json')
    if ($Secret) { $all += '--passphrase-stdin' }
    $all += $Arguments

    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = Find-AVCli
    $info.Arguments = ($all | ForEach-Object { Format-AVArgument $_ }) -join ' '
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.CreateNoWindow = $true
    $info.StandardOutputEncoding = [Text.Encoding]::UTF8
    $info.StandardErrorEncoding = [Text.Encoding]::UTF8
    Write-Verbose "aeternavault-cli $($info.Arguments)"

    $process = [Diagnostics.Process]::Start($info)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if ($Secret) {
        # Written as UTF-8 bytes: Windows PowerShell would otherwise use the
        # console code page, and a passphrase with umlauts would not match.
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        $stream = $process.StandardInput.BaseStream
        foreach ($item in $Secret) {
            $plain = ConvertTo-AVPlainText $item
            try {
                $bytes = $utf8.GetBytes($plain + "`n")
                $stream.Write($bytes, 0, $bytes.Length)
            }
            finally {
                if ($bytes) { [Array]::Clear($bytes, 0, $bytes.Length) }
                $plain = $null
            }
        }
        $stream.Flush()
    }
    $process.StandardInput.Close()
    $process.WaitForExit()
    $out = $stdout.Result
    $err = $stderr.Result.Trim()

    switch ($process.ExitCode) {
        0 { }
        2 { if ($err) { Write-Warning $err } else { Write-Warning 'Completed with problems or not confirmed.' } }
        default {
            $message = if ($err) { $err -replace '^error:\s*', '' } else { "aeternavault-cli exited with code $($process.ExitCode)" }
            throw $message
        }
    }
    if ($Raw) { return $out }
    if ($out.Trim()) {
        $value = $out | ConvertFrom-Json
        # Write arrays element by element so the pipeline sees single objects.
        if ($value -is [array]) { $value | ForEach-Object { $_ } } else { $value }
    }
}

function Add-AVDestination([System.Collections.Generic.List[string]]$List, [string]$Destination) {
    if ($Destination) { $List.Add('--destination'); $List.Add($Destination) }
}

# --- Overview -----------------------------------------------------------------

<#
.SYNOPSIS
Destination, encryption, folders, jobs and the last backup at a glance.
#>
function Get-AVStatus {
    [CmdletBinding()] param()
    Invoke-AVCli @('status')
}

<#
.SYNOPSIS
What AeternaVault did: backups, restores, changes (kept across sessions).
#>
function Get-AVHistory {
    [CmdletBinding()]
    param([int]$Last = 50)
    Invoke-AVCli @('history', '--last', "$Last")
}

<#
.SYNOPSIS
Where configuration, history and logs are stored.
#>
function Get-AVPath {
    [CmdletBinding()] param()
    Invoke-AVCli @('paths')
}

# --- Backups ------------------------------------------------------------------

<#
.SYNOPSIS
Backs up now: all ticked folders, or the folders of one job.
.EXAMPLE
Start-AVBackup
.EXAMPLE
Start-AVBackup -Job 'Evening' -Passphrase (Read-Host -AsSecureString)
#>
function Start-AVBackup {
    [CmdletBinding()]
    param(
        [switch]$Full,
        [string]$Job,
        # Needed for encrypted backups when the key is not remembered.
        [securestring]$Passphrase
    )
    $a = New-Object System.Collections.Generic.List[string]
    $a.Add('backup')
    if ($Full) { $a.Add('--full') }
    if ($Job) { $a.Add('--job'); $a.Add($Job) }
    Invoke-AVCli $a.ToArray() -Secret $Passphrase
}

<#
.SYNOPSIS
Lists the backups, with the date each will be removed by the retention rules.
#>
function Get-AVBackup {
    [CmdletBinding()]
    param([string]$Destination)
    $a = New-Object System.Collections.Generic.List[string]
    $a.Add('list')
    Add-AVDestination $a $Destination
    Invoke-AVCli $a.ToArray()
}

<#
.SYNOPSIS
Lists the files in a backup. Encrypted backups need the passphrase or recovery key.
.EXAMPLE
Get-AVFile latest -Filter letters
#>
function Get-AVFile {
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Backup = 'latest',
        [string]$Filter,
        [securestring]$Passphrase,
        [string]$Destination
    )
    $a = New-Object System.Collections.Generic.List[string]
    $a.Add('files'); $a.Add($Backup)
    if ($Filter) { $a.Add('--filter'); $a.Add($Filter) }
    Add-AVDestination $a $Destination
    Invoke-AVCli $a.ToArray() -Secret $Passphrase
}

<#
.SYNOPSIS
Restores a backup to the original places or into a folder.
.EXAMPLE
Restore-AVBackup latest -To D:\Restored -Only Documents
#>
function Restore-AVBackup {
    [CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
    param(
        [Parameter(Position = 0)][string]$Backup = 'latest',
        [string]$To,
        [string[]]$Only,
        [ValidateSet('ReplaceChanged', 'KeepExisting', 'KeepNewer')][string]$Conflict = 'ReplaceChanged',
        [securestring]$Passphrase,
        [string]$Destination
    )
    $a = New-Object System.Collections.Generic.List[string]
    $a.Add('restore'); $a.Add($Backup)
    if ($To) { $a.Add('--to'); $a.Add($To) }
    foreach ($folder in @($Only | Where-Object { $_ })) { $a.Add('--only'); $a.Add($folder) }
    $a.Add('--conflict')
    $a.Add(($Conflict -creplace '([a-z])([A-Z])', '$1-$2').ToLowerInvariant())
    $a.Add('--yes')
    Add-AVDestination $a $Destination
    $target = if ($To) { $To } else { 'the original places' }
    if ($PSCmdlet.ShouldProcess($target, "Restore backup '$Backup'")) {
        Invoke-AVCli $a.ToArray() -Secret $Passphrase
    }
}

<#
.SYNOPSIS
Copies files out of a backup into a folder (decrypted).
.EXAMPLE
Export-AVFile latest 'Documents/Letters' -To D:\Out
#>
function Export-AVFile {
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Backup = 'latest',
        [Parameter(Position = 1)][string[]]$Path,
        [Parameter(Mandatory)][string]$To,
        [securestring]$Passphrase,
        [string]$Destination
    )
    $a = New-Object System.Collections.Generic.List[string]
    $a.Add('extract'); $a.Add($Backup)
    foreach ($p in @($Path | Where-Object { $_ })) { $a.Add($p) }
    $a.Add('--to'); $a.Add($To)
    Add-AVDestination $a $Destination
    Invoke-AVCli $a.ToArray() -Secret $Passphrase
}

<#
.SYNOPSIS
Reads every file of a backup again and compares it with its checksum.
#>
function Test-AVBackup {
    [CmdletBinding()]
    param(
        [Parameter(Position = 0)][string]$Backup = 'latest',
        [securestring]$Passphrase,
        [string]$Destination
    )
    $a = New-Object System.Collections.Generic.List[string]
    $a.Add('verify'); $a.Add($Backup)
    Add-AVDestination $a $Destination
    Invoke-AVCli $a.ToArray() -Secret $Passphrase
}

<#
.SYNOPSIS
Deletes backups.
#>
function Remove-AVBackup {
    [CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
    param(
        [Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Id')][string[]]$Backup,
        [securestring]$Passphrase
    )
    begin { $all = New-Object System.Collections.Generic.List[string] }
    process { foreach ($b in $Backup) { $all.Add($b) } }
    end {
        if ($all.Count -and $PSCmdlet.ShouldProcess(($all -join ', '), 'Delete backup')) {
            Invoke-AVCli (@('delete') + $all.ToArray() + @('--yes')) -Secret $Passphrase
        }
    }
}

<#
.SYNOPSIS
Moves a backup to another folder or drive.
#>
function Move-AVBackup {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Backup,
        [Parameter(Mandatory)][string]$To,
        [securestring]$Passphrase
    )
    if ($PSCmdlet.ShouldProcess($Backup, "Move to $To")) {
        Invoke-AVCli @('move', $Backup, '--to', $To) -Secret $Passphrase
    }
}

<#
.SYNOPSIS
Removes old backups by the retention rules now.
#>
function Invoke-AVPrune {
    [CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
    param([securestring]$Passphrase)
    if ($PSCmdlet.ShouldProcess('backups outside the retention rules', 'Delete')) {
        Invoke-AVCli @('prune', '--yes') -Secret $Passphrase
    }
}

<#
.SYNOPSIS
Gives a backup a name shown next to its date; an empty name removes it.
#>
function Rename-AVBackup {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Id')][string]$Backup,
        [Parameter(Mandatory, Position = 1)][AllowEmptyString()][string]$Name
    )
    process { Invoke-AVCli @('rename', $Backup, $Name) }
}

<#
.SYNOPSIS
Writes the activity history to a TXT or CSV file.
.EXAMPLE
Export-AVHistory -Path .ctivity.csv -Format Csv
#>
function Export-AVHistory {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Path,
        [ValidateSet('Text', 'Csv', 'Json')][string]$Format = 'Csv',
        [int]$Last = 0
    )
    $full = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    Invoke-AVCli @('history', '--format', $Format.ToLowerInvariant(), '--last', "$Last", '--output', $full) -Raw | Out-Null
    Get-Item -LiteralPath $full
}

# --- Folders and applications -------------------------------------------------

<#
.SYNOPSIS
The folders to back up.
#>
function Get-AVSource {
    [CmdletBinding()] param()
    Invoke-AVCli @('source', 'list')
}

function Add-AVSource {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0, ValueFromPipeline)][string]$Path,
        [string]$Name
    )
    process {
        $full = (Resolve-Path -LiteralPath $Path).ProviderPath
        $a = @('source', 'add', $full)
        if ($Name) { $a += @('--name', $Name) }
        Invoke-AVCli $a
    }
}

<#
.SYNOPSIS
Removes a folder from the list. Nothing is deleted.
#>
function Remove-AVSource {
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Name')][string]$Source)
    process {
        if ($PSCmdlet.ShouldProcess($Source, 'Remove from the folders to back up')) {
            Invoke-AVCli @('source', 'remove', $Source)
        }
    }
}

function Enable-AVSource {
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Name')][string]$Source)
    process { Invoke-AVCli @('source', 'enable', $Source) }
}

function Disable-AVSource {
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Name')][string]$Source)
    process { Invoke-AVCli @('source', 'disable', $Source) }
}

<#
.SYNOPSIS
Leaves out (or includes again) a sub-folder or file of a folder.
.EXAMPLE
Set-AVSourceItem Documents 'Old stuff' -Exclude
#>
function Set-AVSourceItem {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Source,
        [Parameter(Mandatory, Position = 1)][string]$Path,
        [Parameter(Mandatory, ParameterSetName = 'Exclude')][switch]$Exclude,
        [Parameter(Mandatory, ParameterSetName = 'Include')][switch]$Include,
        [Parameter(Mandatory, ParameterSetName = 'Encrypt')][switch]$Encrypt,
        [Parameter(Mandatory, ParameterSetName = 'Plain')][switch]$Plain
    )
    $verb = $PSCmdlet.ParameterSetName.ToLowerInvariant()
    Invoke-AVCli @('source', $verb, $Source, $Path)
}

<#
.SYNOPSIS
Applications found on this computer whose data folders can be backed up.
#>
function Get-AVApp {
    [CmdletBinding()] param()
    Invoke-AVCli @('app', 'list')
}

function Add-AVApp {
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][string]$Id)
    process { Invoke-AVCli @('app', 'add', $Id) }
}

function Remove-AVApp {
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][string]$Id)
    process { Invoke-AVCli @('app', 'remove', $Id) }
}

# --- Jobs ---------------------------------------------------------------------

function Get-AVJob {
    [CmdletBinding()] param()
    Invoke-AVCli @('job', 'list')
}

function ConvertTo-AVJobArgument {
    param([hashtable]$Bound)
    $a = New-Object System.Collections.Generic.List[string]
    if ($Bound.ContainsKey('Every')) { $a.Add('--every'); $a.Add($Bound.Every.ToLowerInvariant()) }
    if ($Bound.ContainsKey('At')) { $a.Add('--at'); $a.Add($Bound.At) }
    if ($Bound.ContainsKey('Day')) { $a.Add('--day'); $a.Add($Bound.Day.ToLowerInvariant()) }
    if ($Bound.ContainsKey('Hours')) { $a.Add('--hours'); $a.Add("$($Bound.Hours)") }
    if ($Bound.ContainsKey('Folder')) { foreach ($f in $Bound.Folder) { $a.Add('--folder'); $a.Add($f) } }
    if ($Bound.ContainsKey('AllFolders') -and $Bound.AllFolders) { $a.Add('--all-folders') }
    if ($Bound.ContainsKey('Name')) { $a.Add('--name'); $a.Add($Bound.Name) }
    if ($Bound.ContainsKey('NoCatchUp') -and $Bound.NoCatchUp) { $a.Add('--no-catch-up') }
    if ($Bound.ContainsKey('Verify')) { if ($Bound.Verify) { $a.Add('--verify') } else { $a.Add('--no-verify') } }
    if ($Bound.ContainsKey('Destination')) { $a.Add('--destination'); $a.Add($(if ($Bound.Destination) { $Bound.Destination } else { '-' })) }
    if ($Bound.ContainsKey('Before')) { $a.Add('--before'); $a.Add($(if ($Bound.Before) { $Bound.Before } else { '-' })) }
    , $a.ToArray()
}

<#
.SYNOPSIS
Creates a backup job.
.EXAMPLE
New-AVJob -Every Day -At 20:00 -Name Evening
.EXAMPLE
New-AVJob -Every Week -Day Sunday -At 10:00 -Folder Documents -Verify
#>
function New-AVJob {
    [CmdletBinding()]
    param(
        [ValidateSet('Day', 'Week', 'Hours', 'Start')][string]$Every = 'Day',
        [ValidatePattern('^\d{1,2}:\d{2}$')][string]$At,
        [ValidateSet('Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday')][string]$Day,
        [ValidateRange(1, 23)][int]$Hours,
        [string[]]$Folder,
        [switch]$AllFolders,
        [string]$Name,
        [switch]$NoCatchUp,
        [switch]$Verify,
        # Back up to this folder instead of the general destination.
        [string]$Destination,
        # A command run before the backup, e.g. pg_dump into a backed-up folder.
        [string]$Before
    )
    $bound = @{} + $PSBoundParameters
    if (-not $bound.ContainsKey('Every')) { $bound.Every = $Every }
    Invoke-AVCli (@('job', 'add') + (ConvertTo-AVJobArgument $bound))
}

<#
.SYNOPSIS
Changes a job; only the given options change.
#>
function Set-AVJob {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Id')][string]$Job,
        [ValidateSet('Day', 'Week', 'Hours', 'Start')][string]$Every,
        [ValidatePattern('^\d{1,2}:\d{2}$')][string]$At,
        [ValidateSet('Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday')][string]$Day,
        [ValidateRange(1, 23)][int]$Hours,
        [string[]]$Folder,
        [switch]$AllFolders,
        [string]$Name,
        [switch]$NoCatchUp,
        [bool]$Verify,
        [string]$Destination,
        [string]$Before
    )
    process {
        $bound = @{} + $PSBoundParameters
        [void]$bound.Remove('Job')
        Invoke-AVCli (@('job', 'edit', $Job) + (ConvertTo-AVJobArgument $bound))
    }
}

function Remove-AVJob {
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Id')][string]$Job)
    process {
        if ($PSCmdlet.ShouldProcess($Job, 'Remove backup job')) { Invoke-AVCli @('job', 'remove', $Job) }
    }
}

function Enable-AVJob {
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Id')][string]$Job)
    process { Invoke-AVCli @('job', 'enable', $Job) }
}

function Disable-AVJob {
    [CmdletBinding()]
    param([Parameter(Mandatory, Position = 0, ValueFromPipelineByPropertyName)][Alias('Id')][string]$Job)
    process { Invoke-AVCli @('job', 'disable', $Job) }
}

<#
.SYNOPSIS
Runs a job now, or with -Due every job that is due.
#>
function Start-AVJob {
    [CmdletBinding(DefaultParameterSetName = 'One')]
    param(
        [Parameter(Mandatory, Position = 0, ParameterSetName = 'One', ValueFromPipelineByPropertyName)][Alias('Id')][string]$Job,
        [Parameter(Mandatory, ParameterSetName = 'Due')][switch]$Due,
        [securestring]$Passphrase
    )
    process {
        if ($Due) { Invoke-AVCli @('job', 'run-due') -Secret $Passphrase }
        else { Invoke-AVCli @('job', 'run', $Job) -Secret $Passphrase }
    }
}

# --- Retention, destination ---------------------------------------------------

function Get-AVRetention {
    [CmdletBinding()] param()
    Invoke-AVCli @('retention', 'show')
}

<#
.SYNOPSIS
Turns the retention rules on or off and sets how many backups are kept.
.EXAMPLE
Set-AVRetention -Enabled $true -KeepLast 5 -Days 14 -Weeks 8 -Months 24
#>
function Set-AVRetention {
    [CmdletBinding()]
    param(
        [bool]$Enabled,
        [int]$KeepLast,
        [int]$Days,
        [int]$Weeks,
        [int]$Months
    )
    if ($PSBoundParameters.ContainsKey('Enabled')) {
        [void](Invoke-AVCli @('retention', $(if ($Enabled) { 'on' } else { 'off' })))
    }
    $a = New-Object System.Collections.Generic.List[string]
    foreach ($pair in @(@('KeepLast', '--keep-last'), @('Days', '--days'), @('Weeks', '--weeks'), @('Months', '--months'))) {
        if ($PSBoundParameters.ContainsKey($pair[0])) { $a.Add($pair[1]); $a.Add("$($PSBoundParameters[$pair[0]])") }
    }
    if ($a.Count) { [void](Invoke-AVCli (@('retention', 'set') + $a.ToArray())) }
    Get-AVRetention
}

function Get-AVDestination {
    [CmdletBinding()] param()
    Invoke-AVCli @('destination', 'show')
}

<#
.SYNOPSIS
Sets where backups are kept. -AppFolder puts them into an AeternaVault folder inside it.
#>
function Set-AVDestination {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Path,
        [switch]$AppFolder
    )
    $full = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    $a = @('destination', 'set', $full)
    if ($AppFolder) { $a += '--app-folder' }
    Invoke-AVCli $a
}

# --- Encryption ---------------------------------------------------------------

function Get-AVEncryption {
    [CmdletBinding()] param()
    Invoke-AVCli @('encryption', 'status')
}

<#
.SYNOPSIS
Creates the encrypted vault at the destination and turns encryption on.
.DESCRIPTION
Returns the recovery key. Keep it somewhere safe, away from the backups:
without the passphrase or the recovery key, encrypted backups cannot be read.
.EXAMPLE
Initialize-AVEncryption -Passphrase (Read-Host 'Passphrase' -AsSecureString) -Remember
#>
function Initialize-AVEncryption {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][securestring]$Passphrase,
        [ValidateSet('XChaCha20', 'AES256')][string]$Cipher = 'XChaCha20',
        [ValidateSet('Standard', 'Strong', 'VeryStrong')][string]$Strength = 'Standard',
        [switch]$Remember,
        [string]$RecoveryFile
    )
    $a = New-Object System.Collections.Generic.List[string]
    $a.AddRange([string[]]@('encryption', 'setup', '--cipher', $Cipher.ToLowerInvariant(), '--strength'))
    $a.Add(($Strength -creplace '([a-z])([A-Z])', '$1-$2').ToLowerInvariant())
    if ($Remember) { $a.Add('--remember') }
    if ($RecoveryFile) {
        $a.Add('--recovery-file')
        $a.Add($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($RecoveryFile))
    }
    if ($PSCmdlet.ShouldProcess((Get-AVDestination).destination, 'Create encrypted vault')) {
        Invoke-AVCli $a.ToArray() -Secret $Passphrase
    }
}

<#
.SYNOPSIS
Turns encryption of new backups on or off, or chooses what is encrypted.
.EXAMPLE
Set-AVEncryption -Enabled $true -Scope Selected
#>
function Set-AVEncryption {
    [CmdletBinding()]
    param(
        [bool]$Enabled,
        [ValidateSet('Everything', 'Selected')][string]$Scope
    )
    if ($PSBoundParameters.ContainsKey('Enabled')) {
        [void](Invoke-AVCli @('encryption', $(if ($Enabled) { 'on' } else { 'off' })))
    }
    if ($Scope) { [void](Invoke-AVCli @('encryption', 'scope', $Scope.ToLowerInvariant())) }
    Get-AVEncryption
}

<#
.SYNOPSIS
Replaces the passphrase. Needs the current passphrase (or the recovery key).
#>
function Set-AVPassphrase {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][securestring]$Current,
        [Parameter(Mandatory)][securestring]$New
    )
    if ($PSCmdlet.ShouldProcess('encrypted vault', 'Change passphrase')) {
        Invoke-AVCli @('encryption', 'change-passphrase') -Secret @($Current, $New)
    }
}

<#
.SYNOPSIS
Checks that a recovery key opens the vault.
#>
function Test-AVRecoveryKey {
    [CmdletBinding()]
    param([Parameter(Mandatory)][securestring]$RecoveryKey)
    Invoke-AVCli @('encryption', 'test-recovery') -Secret $RecoveryKey
}

<#
.SYNOPSIS
Creates a new recovery key; the old one stops working.
#>
function New-AVRecoveryKey {
    [CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
    param(
        [Parameter(Mandatory)][securestring]$Passphrase,
        [string]$RecoveryFile
    )
    $a = @('encryption', 'new-recovery')
    if ($RecoveryFile) {
        $a += @('--recovery-file', $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($RecoveryFile))
    }
    if ($PSCmdlet.ShouldProcess('encrypted vault', 'Replace the recovery key')) {
        Invoke-AVCli $a -Secret $Passphrase
    }
}

<#
.SYNOPSIS
Remembers the key on this computer for automatic backups, or forgets it (-Forget).
.DESCRIPTION
The remembered key is only used to write, list and check backups. Reading
file names or contents always needs the passphrase.
#>
function Set-AVRememberedKey {
    [CmdletBinding(DefaultParameterSetName = 'Remember')]
    param(
        [Parameter(Mandatory, ParameterSetName = 'Remember')][securestring]$Passphrase,
        [Parameter(Mandatory, ParameterSetName = 'Forget')][switch]$Forget
    )
    if ($Forget) { Invoke-AVCli @('encryption', 'forget') }
    else { Invoke-AVCli @('encryption', 'remember') -Secret $Passphrase }
}

# --- Settings and system integration -----------------------------------------

<#
.SYNOPSIS
The whole configuration, or one value.
.EXAMPLE
Get-AVConfig advanced.hardlink_unchanged
#>
function Get-AVConfig {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string]$Key)
    if ($Key) { Invoke-AVCli @('config', 'get', $Key) } else { Invoke-AVCli @('config', 'show') }
}

<#
.SYNOPSIS
Changes one setting.
.EXAMPLE
Set-AVConfig language de
#>
function Set-AVConfig {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)][string]$Key,
        [Parameter(Mandatory, Position = 1)][AllowEmptyString()][string]$Value
    )
    Invoke-AVCli @('config', 'set', $Key, $Value)
}

<#
.SYNOPSIS
Shows or changes how AeternaVault is integrated with the system.
.DESCRIPTION
-Background: run backup jobs in the background (Windows: start with Windows;
Linux: systemd user timer). -DoubleClick and -ExplorerMenu are Windows only.
.EXAMPLE
Set-AVSystem -Background $true
#>
function Set-AVSystem {
    [CmdletBinding()]
    param(
        [bool]$Background,
        [bool]$DoubleClick,
        [bool]$ExplorerMenu
    )
    $map = @{ Background = 'background'; DoubleClick = 'double-click'; ExplorerMenu = 'explorer-menu' }
    foreach ($name in $map.Keys) {
        if ($PSBoundParameters.ContainsKey($name)) {
            $state = if ($PSBoundParameters[$name]) { 'on' } else { 'off' }
            Invoke-AVCli @('system', $map[$name], $state)
        }
    }
}

function Get-AVSystem {
    [CmdletBinding()] param()
    $names = @('background')
    if ([Environment]::OSVersion.Platform -eq 'Win32NT') { $names += @('double-click', 'explorer-menu') }
    foreach ($name in $names) {
        Invoke-AVCli @('system', $name)
    }
}

Export-ModuleMember -Function @(
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
