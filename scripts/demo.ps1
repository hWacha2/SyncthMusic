# Сквозная демонстрация: два «устройства», одна библиотека, ноль серверов.
# Запуск:  powershell -ExecutionPolicy Bypass -File scripts\demo.ps1

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
# Убираем INFO-шум транспорта из вывода демо.
$env:RUST_LOG = 'warn'

$root = Split-Path $PSScriptRoot -Parent
$exe  = Join-Path $root 'target\release\syncth.exe'
if (-not (Test-Path $exe)) { throw "Сначала соберите: cargo build --release" }

$demo = Join-Path $root '.demo'
if (Test-Path $demo) { Remove-Item -Recurse -Force $demo }

$musicA = Join-Path $demo 'music-a'
$musicB = Join-Path $demo 'music-b'
$dataA  = Join-Path $demo 'data-a'
$dataB  = Join-Path $demo 'data-b'
$logs   = Join-Path $demo 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null

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

# Запускает сервер, ждёт порт и возвращает процесс.
function Start-Server($dataDir, $port, $token) {
    $log = Join-Path $logs "server-$port.log"
    $p = Start-Process -PassThru -WindowStyle Hidden `
        -FilePath $exe `
        -ArgumentList @('--data-dir', "`"$dataDir`"", 'serve', '--bind', "127.0.0.1:$port", '--token', $token) `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    for ($i = 0; $i -lt 100; $i++) {
        try {
            $c = New-Object System.Net.Sockets.TcpClient
            $c.Connect('127.0.0.1', $port)
            $c.Close()
            return $p
        } catch {
            Start-Sleep -Milliseconds 100
        }
    }
    throw "сервер на порту $port не поднялся"
}

function Stop-Server($p) {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 300
}

function Sync-Pair($serverData, $clientData, $port, $token) {
    $p = Start-Server $serverData $port $token
    try {
        Run-Syncth $clientData "sync (LAN :$port)" @('sync', '--addr', "127.0.0.1:$port", '--token', $token)
    } finally {
        Stop-Server $p
    }
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
$token = (& $exe --data-dir $dataA token).Trim()
Write-Host "токен библиотеки: $token" -ForegroundColor DarkGray

# ---------------------------------------------------------------- ПК (B)
Write-Host ""
Write-Host "=== Устройство B: ПК, папка пустая ===" -ForegroundColor Green
New-Item -ItemType Directory -Force -Path $musicB | Out-Null
Run-Syncth $dataB 'init --root music-b' @('init', '--root', $musicB)

Write-Host ""
Write-Host "=== Передача по Wi-Fi (TCP, без сервера и облака) ===" -ForegroundColor Green
Sync-Pair $dataA $dataB 47890 $token

$stateB = Get-State $dataB
if ($stateA -ne $stateB) { throw "манифест не сошёлся: $stateA != $stateB" }
$onB = Get-ChildItem -Recurse -File $musicB
if ($onB.Count -ne 3) { throw "ожидали 3 файла на B, получили $($onB.Count)" }
Write-Host ""
Write-Host "OK: 3 файла приехали, state_hash совпал, mtime сохранён" -ForegroundColor DarkGray

# ---------------------------------------------------------------- правка
Write-Host ""
Write-Host "=== Правка тегов на ПК: файл меняется и уезжает на телефон ===" -ForegroundColor Green
$edited = Join-Path $musicB 'Boards of Canada\1998 - Music Has the Right to Children\02 - An Eagle in Your Mind.flac'
[System.IO.File]::AppendAllText($edited, "TAG-EDIT")
Run-Syncth $dataB 'scan (после правки)' @('scan')
Sync-Pair $dataB $dataA 47891 $token

$copies = Get-ChildItem -Recurse -File $musicA | Where-Object { $_.Name -like '*sync-conflict*' }
if ($copies.Count -ne 0) { throw "обычная правка не должна плодить конфликтные копии" }
$stateA = Get-State $dataA
$stateB = Get-State $dataB
if ($stateA -ne $stateB) { throw "правка не доехала" }
Write-Host ""
Write-Host "OK: правка доехала, конфликтной копии нет (это обновление, а не конфликт)" -ForegroundColor DarkGray

# ---------------------------------------------------------------- удаление
Write-Host ""
Write-Host "=== Удаление файла на телефоне ===" -ForegroundColor Green
$gone = Join-Path $musicA 'БГ\2008 - Русский альбом\01 - Поезд в огне.flac'
Remove-Item $gone
Run-Syncth $dataA 'scan (файл пропал)' @('scan')
Run-Syncth $dataA 'pending list' @('pending', 'list')

Write-Host ""
Write-Host "Файл на ПК пока на месте — удаление ждёт решения." -ForegroundColor DarkGray
Write-Host ""
Write-Host "=== Решение: удалить везде ===" -ForegroundColor Green
Run-Syncth $dataA 'pending resolve --delete-everywhere' `
    @('pending', 'resolve', 'БГ/2008 - Русский альбом/01 - Поезд в огне.flac', '--delete-everywhere')

Sync-Pair $dataA $dataB 47892 $token

$onPC = Join-Path $musicB 'БГ\2008 - Русский альбом\01 - Поезд в огне.flac'
if (Test-Path $onPC) { throw "подтверждённое удаление не доехало до ПК" }
if (-not (Test-Path (Join-Path $musicB 'Boards of Canada\1998 - Music Has the Right to Children\02 - An Eagle in Your Mind.flac'))) {
    throw "удалили не тот файл"
}
$stateA = Get-State $dataA
$stateB = Get-State $dataB
if ($stateA -ne $stateB) { throw "после удаления состояние разошлось" }

Write-Host ""
Write-Host "OK: удаление подтверждено на телефоне и применилось на ПК." -ForegroundColor Green
Write-Host "Остальные файлы целы, state_hash совпадает: $stateA" -ForegroundColor DarkGray
Write-Host ""
Write-Host "Демо завершено." -ForegroundColor Green
