# Removes Ferry for the current user. Settings and pairings in %APPDATA%\Ferry are kept.
$dir = Join-Path $env:LOCALAPPDATA 'Programs\Ferry'
Get-Process ferryd -ErrorAction SilentlyContinue | Stop-Process -Force
Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Ferry' -ErrorAction SilentlyContinue
Remove-Item (Join-Path $env:APPDATA 'Microsoft\Windows\SendTo\Phone (Ferry).lnk') -ErrorAction SilentlyContinue
Remove-Item (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Ferry.lnk') -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
$p = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($p) { [Environment]::SetEnvironmentVariable('Path', (($p -split ';') | Where-Object { $_ -and $_ -ne $dir }) -join ';', 'User') }
Write-Host 'Ferry removed. (Firewall rules "Ferry" can be deleted in Windows Defender Firewall settings.)'
Write-Host "Settings remain in $env:APPDATA\Ferry"
