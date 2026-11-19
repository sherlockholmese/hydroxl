param([switch]$AfterRestart, [int]$LegacyStart)

$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $arguments = '-NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $PSCommandPath
    $process = Start-Process -FilePath "$PSHOME\powershell.exe" -ArgumentList $arguments `
        -Verb RunAs -Wait -PassThru
    exit $process.ExitCode
}

$taskName = 'Hydroxl Install Triton'
$legacyKey = 'HKLM:\SYSTEM\CurrentControlSet\Services\VioGpuDod'
$fallbackKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\BasicDisplay'
$setupDir = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Hydroxl\TritonSetup'
$stagedDriver = Join-Path $setupDir 'viogpu3d'
$stagedScript = Join-Path $setupDir 'Install-Triton.ps1'
$log = Join-Path $env:SystemRoot 'Logs\Hydroxl-Triton.log'

function Test-LegacyDriverRunning {
    $driver = Get-CimInstance Win32_SystemDriver -Filter "Name='VioGpuDod'"
    return [bool]($driver -and $driver.Started)
}

function Enable-BasicDisplayFallback {
    $value = 'DisableBasicDisplayFallback'
    if (Get-ItemProperty -LiteralPath $fallbackKey -Name $value -ErrorAction SilentlyContinue) {
        Remove-ItemProperty -LiteralPath $fallbackKey -Name $value
    }
}

function Invoke-PnpUtil([string[]]$Arguments, [switch]$AllowAlreadyInstalled) {
    & "$env:SystemRoot\System32\pnputil.exe" @Arguments | Out-Host
    $result = $LASTEXITCODE
    if ($result -eq 3010) { return $true }
    if ($result -eq 0 -or ($result -eq 259 -and $AllowAlreadyInstalled)) { return $false }
    throw "PnPUtil failed (exit code $result)."
}

function Install-Triton([string]$Inf) {
    if (Test-LegacyDriverRunning) {
        throw 'The legacy VirtIO display driver is still running.'
    }
    Enable-BasicDisplayFallback
    $requiresRestart = $false
    $legacyPackages = Get-WindowsDriver -Online | Where-Object {
        (Split-Path -Leaf $_.OriginalFileName) -ieq 'viogpudo.inf'
    }
    foreach ($package in $legacyPackages) {
        Write-Host "Removing $($package.Driver) (viogpudo.inf)."
        if (Invoke-PnpUtil @('/delete-driver', $package.Driver, '/uninstall')) {
            $requiresRestart = $true
        }
    }
    $adapters = @(Get-PnpDevice -PresentOnly | Where-Object {
        $_.InstanceId -like 'PCI\VEN_1AF4&DEV_1050*'
    })
    $arguments = @('/add-driver', $Inf)
    if ($adapters.Count -gt 0) { $arguments += '/install' }
    # PnPUtil can report 259 if an adapter already has an equal or better driver.
    $otherDrivers = @($adapters | Where-Object {
        $property = Get-PnpDeviceProperty -InstanceId $_.InstanceId `
            -KeyName DEVPKEY_Device_Service -ErrorAction SilentlyContinue
        $property.Data -ine 'VioGpu3D'
    })
    $tritonAlreadyBound = $adapters.Count -gt 0 -and $otherDrivers.Count -eq 0
    if (Invoke-PnpUtil $arguments -AllowAlreadyInstalled:$tritonAlreadyBound) {
        $requiresRestart = $true
    }
    Write-Host 'Triton is ready. Enable VirtIO and DirectX in Hydroxl before the next accelerated boot.'
    if ($requiresRestart) {
        Write-Host 'Windows also requested a restart to finish applying the driver changes.'
    }
}

function Queue-Install([string]$DriverDir) {
    $previousStart = (Get-ItemProperty -LiteralPath $legacyKey -Name Start).Start
    New-Item -ItemType Directory -Path $stagedDriver -Force | Out-Null
    Copy-Item -Path (Join-Path $DriverDir '*') -Destination $stagedDriver -Recurse -Force
    Copy-Item -LiteralPath $PSCommandPath -Destination $stagedScript -Force
    Enable-BasicDisplayFallback

    $arguments = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -AfterRestart -LegacyStart {1}' -f `
        $stagedScript, $previousStart
    $action = New-ScheduledTaskAction -Execute "$PSHOME\powershell.exe" -Argument $arguments
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger `
        -Settings $settings -User SYSTEM -RunLevel Highest | Out-Null
    try {
        # The old driver can hang PnP when unloaded. Disable it on the next boot
        # before asking PnP to remove the package.
        Set-ItemProperty -LiteralPath $legacyKey -Name Start -Value 4
    } catch {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
        throw
    }
    Write-Host 'Restart Windows to finish installing Triton automatically.'
}

if ($AfterRestart -and -not $identity.IsSystem) {
    throw 'The post-restart installer runs through Windows Task Scheduler.'
}

$lock = $null
$transcript = $false
$exitCode = 0
try {
    New-Item -ItemType Directory -Path $setupDir -Force | Out-Null
    $lock = [IO.File]::Open((Join-Path $setupDir 'install.lock'), 'OpenOrCreate', 'Write', 'None')
    Start-Transcript -Path $log -Append | Out-Null
    $transcript = $true
    if ($AfterRestart) {
        try {
            Install-Triton (Join-Path $stagedDriver 'viogpu3d.inf')
        } catch {
            if (Test-Path -LiteralPath $legacyKey) {
                Set-ItemProperty -LiteralPath $legacyKey -Name Start -Value $LegacyStart
            }
            throw
        } finally {
            Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
        }
        Remove-Item -LiteralPath $stagedDriver, $stagedScript -Recurse -Force
    } elseif (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Write-Host 'The installer is already scheduled. Restart Windows to continue.'
    } else {
        $driverDir = Join-Path $PSScriptRoot 'drivers\viogpu3d'
        $inf = Join-Path $driverDir 'viogpu3d.inf'
        if (-not (Test-Path -LiteralPath $inf -PathType Leaf)) {
            throw 'The Triton driver package is missing. Run this installer from the Triton disc.'
        }
        if (Test-LegacyDriverRunning) {
            Queue-Install $driverDir
        } else {
            Install-Triton $inf
        }
    }
} catch {
    Write-Host "Triton setup failed. $($_.Exception.Message)"
    Write-Host "Setup log $log"
    $exitCode = 1
} finally {
    if ($transcript) { Stop-Transcript | Out-Null }
    if ($lock) { $lock.Dispose() }
}
if (-not $AfterRestart) { $null = Read-Host 'Press Enter to close' }
exit $exitCode
