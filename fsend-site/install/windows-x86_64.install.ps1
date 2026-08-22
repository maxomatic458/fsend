$root = "$env:LOCALAPPDATA\fsend"
$dir  = "$root\fsend-x86_64-pc-windows-msvc"

Invoke-WebRequest "https://github.com/maxomatic458/fsend/releases/latest/download/fsend-x86_64-pc-windows-msvc.zip" -OutFile "$env:TEMP\fsend.zip" -UseBasicParsing
Expand-Archive "$env:TEMP\fsend.zip" -DestinationPath $root -Force

$path = [Environment]::GetEnvironmentVariable("Path", "User")
if ($path -notlike "*$dir*") {
  [Environment]::SetEnvironmentVariable("Path", "$path;$dir", "User")
}

& "$dir\fsend.exe" version
