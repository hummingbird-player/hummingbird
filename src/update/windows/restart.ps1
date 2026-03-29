# pretty much a direct adaptation of macOS's `restart.sh` but for Windows

param(
    [Parameter(Mandatory = $true)]
    [string]$AppPid,

    [Parameter(Mandatory = $true)]
    [string]$SourceApp,

    [Parameter(Mandatory = $true)]
    [string]$TargetApp,

    [Parameter(Mandatory = $true)]
    [string]$TempRoot
)

$ErrorActionPreference = "Stop"

$installDir = Split-Path -Parent $TargetApp
$appName = Split-Path -Leaf $TargetApp
$stagedApp = Join-Path $installDir ".$appName.update"
$backupApp = Join-Path $installDir ".$appName.backup"

function Remove-PathIfExists
{
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (Test-Path -LiteralPath $Path)
    {
        Remove-Item -LiteralPath $Path -Force -Recurse
    }
}

# wait until hummingbird exits
while ($true)
{
    $process = Get-Process -Id $AppPid -ErrorAction SilentlyContinue
    if ($null -eq $process)
    {
        break
    }

    Start-Sleep -Seconds 1
}

# copy the downloaded executable into a staged location
Remove-PathIfExists -Path $stagedApp
Remove-PathIfExists -Path $backupApp
Copy-Item -LiteralPath $SourceApp -Destination $stagedApp -Force

# move the current app out of the way, then swap the staged app into place
if (Test-Path -LiteralPath $TargetApp)
{
    Move-Item -LiteralPath $TargetApp -Destination $backupApp -Force
}

# restore the backup if the swap fails, or delete it if it's successful
try
{
    Move-Item -LiteralPath $stagedApp -Destination $TargetApp -Force

    if (Test-Path -LiteralPath $backupApp)
    {
        Remove-Item -LiteralPath $backupApp -Force
    }
} catch
{
    if (Test-Path -LiteralPath $backupApp)
    {
        Move-Item -LiteralPath $backupApp -Destination $TargetApp -Force
    }

    throw
}

# remove the temporary files and relaunch
Remove-PathIfExists -Path $TempRoot
Start-Process -FilePath $TargetApp
