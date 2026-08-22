$root = "$env:LOCALAPPDATA\fsend"
$dir  = "$root\fsend-x86_64-pc-windows-msvc"
Remove-Item $root -Recurse -Force

$path = [Environment]::GetEnvironmentVariable("Path", "User")
[Environment]::SetEnvironmentVariable(
  "Path", ($path -split ';' | Where-Object { $_ -ne $dir }) -join ';', "User")
