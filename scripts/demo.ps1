# Сквозная демонстрация Phase 0: два «устройства», одна библиотека, ноль серверов.
# Запуск:  powershell -ExecutionPolicy Bypass -File scripts\demo.ps1

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

$root = Split-Path $PSScriptRoot -Parent
$exe  = Join-Path $root 'target\release\syncth.exe'
if (-not (Test-Path $exe)) { throw "Сначала соберите: cargo build --release" }

$demo = Join-Path $root '.demo'
if (Test-Path $demo) { Remove-Item -Recurse -Force $demo }

$musicA = Join-Path $demo 'music-a'
$musicB = Join-Path $demo 'music-b'
$dataA  = Join-Path $demo 'data-a'
$dataB  = Join-Path $demo 'data-b'
$pipe   = Join-Path $demo 'pipe'

function Run-Syncth($dataDir, $label, [string[]]$argv) {
    Write-Host ""
    Write-Host "> syncth $label" -ForegroundColor Cyan
    & $exe --data-dir $dataDir @argv
    if ($LASTEXITCODE -ne 0) { throw "syncth $label завершился с кодом $LASTEXITCODE" }
}

function Get-State($dataDir) {
    $out = & $exe --data-dir $dataDir status
    foreach ($line in $out) {
        if ($line -match 'state:\s+(\S+)') { return $Matches[1] }
    }
    throw "не удалось прочитать state у $dataDir"
}

function New-Track($dir, $name, $seed, $count) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $bytes = [byte[]]::new($count * 64)
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = ($i * $seed + $seed) % 251 }
    [System.IO.File]::WriteAllBytes((Join-Path $dir $name), $bytes)
}

# ---------------------------------------------------------------- телефон (A)
Write-Host "=== Устройство A: телефон, музыка уже есть ===" -ForegroundColor Green
New-Track (Join-Path $musicA 'Boards of Canada\1998 - Music Has the Right to Children') `
          '01 - Wildlife Analysis.flac' 7 400
New-Track (Join-Path $musicA 'Boards of Canada\1998 - Music Has the Right to Children') `
          '02 - An Eagle in Your Mind.flac' 13 900
New-Track (Join-Path $musicA 'БГ\2008 - Русский альбом') '01 - Поезд в огне.flac' 42 700

Run-Syncth $dataA 'init --root music-a' @('init', '--root', $musicA)
Run-Syncth $dataA 'scan' @('scan')
$stateA = Get-State $dataA

# ---------------------------------------------------------------- ПК (B)
Write-Host ""
Write-Host "=== Устройство B: ПК, папка пустая ===" -ForegroundColor Green
New-Item -ItemType Directory -Force -Path $musicB | Out-Null
Run-Syncth $dataB 'init --root music-b' @('init', '--root', $musicB)

# ---------------------------------------------------------------- манифест
Run-Syncth $dataA 'export --dir pipe' @('export', '--dir', $pipe)
Run-Syncth $dataB 'import --dir pipe' @('import', '--dir', $pipe)
Run-Syncth $dataB 'scan (файлов ещё нет)' @('scan')

$stateB = Get-State $dataB
if ($stateA -ne $stateB) { throw "манифест не сошёлся: $stateA != $stateB" }
Write-Host "OK: манифест одинаковый, но файлов на B нет — удалением это не считается" -ForegroundColor DarkGray

# ---------------------------------------------------------------- файлы
Write-Host ""
Write-Host "=== Передача файлов (в Phase 0 вручную, Phase 0.1 — по сети) ===" -ForegroundColor Green
Copy-Item -Path (Join-Path $musicA '*') -Destination $musicB -Recurse -Force
Run-Syncth $dataB 'scan (файлы приехали)' @('scan')
Run-Syncth $dataB 'export --dir pipe' @('export', '--dir', $pipe)
Run-Syncth $dataA 'import --dir pipe' @('import', '--dir', $pipe)

$stateA = Get-State $dataA
$stateB = Get-State $dataB
if ($stateA -ne $stateB) { throw "после передачи состояние разошлось" }
Write-Host "OK: state_hash совпадает: $stateA" -ForegroundColor DarkGray

# ---------------------------------------------------------------- правка
Write-Host ""
Write-Host "=== Правка тегов на телефоне (файл меняется) ===" -ForegroundColor Green
$edited = Join-Path $musicB 'Boards of Canada\1998 - Music Has the Right to Children\02 - An Eagle in Your Mind.flac'
[System.IO.File]::AppendAllText($edited, "TAG-EDIT")
Run-Syncth $dataB 'scan (после правки)' @('scan')
Run-Syncth $dataB 'export --dir pipe' @('export', '--dir', $pipe)
Run-Syncth $dataA 'import --dir pipe' @('import', '--dir', $pipe)

$stateA = Get-State $dataA
$stateB = Get-State $dataB
if ($stateA -ne $stateB) { throw "правка не доехала" }

# ---------------------------------------------------------------- удаление
Write-Host ""
Write-Host "=== Удаление файла на телефоне ===" -ForegroundColor Green
Remove-Item (Join-Path $musicB 'БГ\2008 - Русский альбом\01 - Поезд в огне.flac')
Run-Syncth $dataB 'scan (файл пропал)' @('scan')
Run-Syncth $dataB 'status' @('status')

Run-Syncth $dataB 'export --dir pipe' @('export', '--dir', $pipe)
Run-Syncth $dataA 'import --dir pipe' @('import', '--dir', $pipe)
Run-Syncth $dataA 'status' @('status')

$onA = Join-Path $musicA 'БГ\2008 - Русский альбом\01 - Поезд в огне.flac'
if (-not (Test-Path $onA)) { throw "ФАЙЛ НА A ИСЧЕЗ! удаление не должно распространяться" }
Write-Host ""
Write-Host "OK: на телефоне файл удалён, на ПК файл на месте, удаление ждёт решения." -ForegroundColor Green
Write-Host "Демо завершено." -ForegroundColor Green
