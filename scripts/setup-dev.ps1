# Требует прав администратора (winget ставит пакеты в системные каталоги).
# Запуск:
#   powershell -ExecutionPolicy Bypass -File scripts\setup-dev.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\setup-dev.ps1 -WithFlutter

param(
  [switch]$WithFlutter
)

$ErrorActionPreference = 'Stop'

function Test-Cmd($name) { return [bool](Get-Command $name -ErrorAction SilentlyContinue) }

function Install-Pkg($id, $extra) {
  if ($extra) {
    Write-Host "-> winget install $id (with args)" -ForegroundColor Cyan
    winget install --id $id -e --accept-package-agreements --accept-source-agreements --disable-interactivity --override $extra
  } else {
    Write-Host "-> winget install $id" -ForegroundColor Cyan
    winget install --id $id -e --accept-package-agreements --accept-source-agreements --disable-interactivity
  }
}

# ---------------------------------------------------------------- база (Phase 0)
if (-not (Test-Cmd 'rustc')) { Install-Pkg 'Rustlang.Rustup' } else { Write-Host "rust: OK" }

# MSVC linker + Windows SDK + CMake + Ninja одним пакетом.
# Нужен и для Rust (линковка), и для Flutter Windows (CMake-сборка).
$vs = 'C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\devenv.exe'
if (-not (Test-Path $vs)) {
  Install-Pkg 'Microsoft.VisualStudio.2022.Community' `
    '--wait --quiet --norestart --add Microsoft.VisualStudio.Workload.NativeDesktop --includeRecommended'
} else { Write-Host "Visual Studio 2022: OK" }

foreach ($p in @(
  @{ id = 'Kitware.CMake';          cmd = 'cmake' },
  @{ id = 'Ninja-build.Ninja';      cmd = 'ninja' },
  @{ id = 'LLVM.LLVM';              cmd = 'clang' },
  @{ id = 'Microsoft.VisualStudioCode'; cmd = 'code' },
  @{ id = 'DBBrowserForSQLite.DBBrowserForSQLite'; cmd = $null }
)) {
  if (-not $p.cmd -or -not (Test-Cmd $p.cmd)) { Install-Pkg $p.id }
}

# Git обычно уже есть, но пусть будет.
if (-not (Test-Cmd 'git')) { Install-Pkg 'Git.Git' }

# ---------------------------------------------------------------- инструменты
Write-Host "-> rustup default stable-x86_64-pc-windows-msvc" -ForegroundColor Cyan
rustup default stable-x86_64-pc-windows-msvc
rustup component add rust-analyzer clippy rustfmt

Write-Host "-> cargo tools" -ForegroundColor Cyan
cargo install cargo-nextest cargo-watch bacon

# ---------------------------------------------------------------- Flutter (Phase 1)
if ($WithFlutter) {
  $flutterDir = 'F:\dev\flutter'
  if (-not (Test-Path "$flutterDir\bin\flutter.bat")) {
    Write-Host "-> git clone flutter stable -> $flutterDir" -ForegroundColor Cyan
    New-Item -ItemType Directory -Force -Path (Split-Path $flutterDir) | Out-Null
    git clone --depth 1 -b stable https://github.com/flutter/flutter.git $flutterDir
  }
  $env:Path = "$flutterDir\bin;$env:Path"

  if (-not (Test-Cmd 'adb')) { Install-Pkg 'Google.AndroidStudio' }

  Write-Host "-> Flutter doctor" -ForegroundColor Cyan
  flutter doctor -v

  Write-Host "-> Rust targets + bridge" -ForegroundColor Cyan
  cargo install --locked flutter_rust_bridge_codegen cargo-ndk
  rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
}

# ---------------------------------------------------------------- проверка
Write-Host "`n=== Проверка ===" -ForegroundColor Green
foreach ($t in 'rustc','cargo','cmake','ninja','clang','git','code') {
  if (Test-Cmd $t) { "  {0,-6} {1}" -f $t, (Get-Command $t).Source } else { "  {0,-6} НЕ НАЙДЕН (перезапусти терминал)" -f $t }
}
Write-Host "`nНе забудь перезапустить терминал, чтобы подхватился PATH." -ForegroundColor Yellow
