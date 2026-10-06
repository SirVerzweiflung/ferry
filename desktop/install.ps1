# Builds and installs Ferry for the current Windows user. Run via install.cmd (double-click).
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Set-Location $PSScriptRoot
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Ferry'

function Have($cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

# ---- Rust --------------------------------------------------------------------
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (-not (Have cargo) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) { $env:Path = "$cargoBin;$env:Path" }
if (-not (Have cargo)) {
    Write-Host '==> Installing Rust (one time)'
    $init = Join-Path $env:TEMP 'rustup-init.exe'
    Invoke-WebRequest 'https://win.rustup.rs/x86_64' -OutFile $init
    # The GNU toolchain brings its own linker, so no Visual Studio is needed.
    & $init -y --profile minimal --default-toolchain stable-x86_64-pc-windows-gnu | Out-Host
    $env:Path = "$cargoBin;$env:Path"
}

# The MSVC toolchain needs the Visual Studio C++ build tools. Without them, use GNU.
$toolchain = @()
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$hasMsvc = (Test-Path $vswhere) -and (& $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
$host_ = (& rustc -vV | Select-String '^host:').ToString()
if ($host_ -like '*msvc*' -and -not $hasMsvc) {
    Write-Host '==> No Visual Studio build tools found - using the GNU toolchain instead'
    & rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal | Out-Host
    $toolchain = @('+stable-x86_64-pc-windows-gnu')
}

Write-Host '==> Building (no external crates)'
& cargo @toolchain build --release
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

# ---- install -----------------------------------------------------------------
Write-Host "==> Installing to $dir"
Get-Process ferryd -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item 'target\release\ferry.exe', 'target\release\ferryd.exe' $dir -Force
$exe = Join-Path $dir 'ferryd.exe'

# 'ferry' command in new terminals
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
if ($userPath -notlike "*$dir*") {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$dir").TrimStart(';'), 'User')
}

# Start at login (tray icon)
Set-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Ferry' -Value "`"$exe`""

# Explorer: right-click -> Send to -> Phone (Ferry); and a Start menu entry
$sh = New-Object -ComObject WScript.Shell
$lnk = $sh.CreateShortcut((Join-Path $env:APPDATA 'Microsoft\Windows\SendTo\Phone (Ferry).lnk'))
$lnk.TargetPath = $exe; $lnk.Arguments = 'send'; $lnk.Description = 'Send to your phone with Ferry'; $lnk.Save()
$lnk = $sh.CreateShortcut((Join-Path $env:APPDATA 'Microsoft\Windows\SendTo\Ferry (choose device).lnk'))
$lnk.TargetPath = $exe; $lnk.Arguments = 'send --choose'; $lnk.Description = 'Send with Ferry to a device of your choice'; $lnk.Save()
$lnk = $sh.CreateShortcut((Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Ferry.lnk'))
$lnk.TargetPath = $exe; $lnk.Description = 'Ferry - share files and clipboard with your phone'; $lnk.Save()

# Firewall: allow incoming connections on private networks (one admin prompt)
if (-not (Get-NetFirewallRule -DisplayName 'Ferry' -ErrorAction SilentlyContinue)) {
    Write-Host '==> Allowing Ferry through the Windows firewall (confirm the admin prompt)'
    $cmd = "New-NetFirewallRule -DisplayName 'Ferry' -Direction Inbound -Action Allow -Profile Private,Domain -Program '$exe' | Out-Null; " +
           "New-NetFirewallRule -DisplayName 'Ferry CLI' -Direction Inbound -Action Allow -Profile Private,Domain -Program '$(Join-Path $dir 'ferry.exe')' | Out-Null"
    try {
        Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '-NoProfile', '-Command', $cmd
    } catch {
        Write-Host '    Skipped. Windows will ask on first use instead - allow "Private networks".'
    }
}

Start-Process $exe
Write-Host ''
Write-Host 'Ferry is installed and running (look for the icon in the system tray, maybe under ^).'
Write-Host ' * Pair: click the tray icon -> "Pair new phone..."'
Write-Host ' * Send files: right-click files in Explorer -> Send to -> Phone (Ferry)'
Write-Host ' * Your Wi-Fi must be set to "Private network" in Windows settings, or the phone cannot connect.'
