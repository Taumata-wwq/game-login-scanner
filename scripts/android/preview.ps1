# 浏览器预览：启动本地静态服务器（src 目录），浏览器打开即可预览 UI（无需构建 APK）。
#
# 用法：pwsh -File scripts/android/preview.ps1 [-Port 1430] [-Mobile]
param(
    [int]$Port = 1430,
    [switch]$Mobile
)
$ErrorActionPreference = "Stop"
$src = Join-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) "src"
$url = "http://localhost:$Port/index.html" + $(if ($Mobile) { "?mobile" } else { "" })
Write-Host "预览目录: $src"
Write-Host "预览地址: $url"
Write-Host "（移动端布局加 ?mobile；桌面端加 ?desktop；Ctrl+C 停止）"
Write-Host ""
Start-Process $url
# Python 内置 http.server（Python 3.12 已在 PATH）
Push-Location $src
try { python -m http.server $Port } finally { Pop-Location }
