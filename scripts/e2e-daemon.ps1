# Временная проверка: сопряжение, низкий диск, UI API.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:RUST_LOG = 'warn'

$root = Split-Path $PSScriptRoot -Parent
$cli = Join-Path $root 'target\release\syncth.exe'
$dmn = Join-Path $root 'target\release\syncthd.exe'
$base = Join-Path $root '.e2e'
if (Test-Path $base) { Remove-Item -Recurse -Force $base }
New-Item -ItemType Directory -Force $base | Out-Null

$mA = Join-Path $base 'music-a'
$mB = Join-Path $base 'music-b'
$dA = Join-Path $base 'data-a'
$dB = Join-Path $base 'data-b'
New-Item -ItemType Directory -Force $mA, $mB | Out-Null

function Track($dir, $name) {
    $bytes = [byte[]]::new(5000)
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = ($i * 7) % 251 }
    $full = Join-Path $dir $name
    New-Item -ItemType Directory -Force (Split-Path $full) | Out-Null
    [System.IO.File]::WriteAllBytes($full, $bytes)
}
Track $mA 'A\01 - First.flac'

& $cli --data-dir $dA init --root $mA | Out-Null
& $cli --data-dir $dA scan | Out-Null
& $cli --data-dir $dB init --root $mB | Out-Null

# Строка сопряжения с адресом A.
$code = (& $cli --data-dir $dA pair-code --addr '127.0.0.1:47900' | Select-Object -First 1).Trim()
if (-not $code.StartsWith('syncth://pair?')) { throw "pair-code: $code" }
& $cli --data-dir $dB pair $code | Out-Null
$peers = & $cli --data-dir $dB peers
if (-not ($peers -join "`n").Contains('127.0.0.1:47900')) { throw "peers: $peers" }
Write-Host "OK: строка сопряжения разобрана, адрес запомнен" -ForegroundColor Green

function Start-Daemon($data, $bind, $ui, [string[]]$extra) {
    $log = Join-Path $base ("d-" + $bind.Replace(':', '-') + '.log')
    $args = @('--data-dir', "`"$data`"", '--bind', $bind, '--ui', $ui, '--no-tray', '--no-notify') + $extra
    $p = Start-Process -PassThru -WindowStyle Hidden -FilePath $dmn -ArgumentList $args -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    for ($i = 0; $i -lt 100; $i++) {
        try {
            $r = Invoke-RestMethod -Uri "http://$ui/api/status" -TimeoutSec 1
            return $p
        } catch { Start-Sleep -Milliseconds 100 }
    }
    throw "UI $ui не поднялся"
}

$pa = Start-Daemon $dA '127.0.0.1:47900' '127.0.0.1:47901' @()
$pb = Start-Daemon $dB '127.0.0.1:47902' '127.0.0.1:47903' @('--sync-interval', '5')
try {
    $sa = Invoke-RestMethod -Uri 'http://127.0.0.1:47901/api/status'
    if ($sa.state -ne 'running') { throw "state A: $($sa.state)" }
    $pc = Invoke-RestMethod -Uri 'http://127.0.0.1:47901/api/pair-code'
    if (-not $pc.code.StartsWith('syncth://pair?')) { throw "UI pair-code: $($pc.code)" }
    if (-not $pc.svg.Contains('<svg')) { throw 'QR SVG пуст' }
    Write-Host "OK: UI отдаёт строку сопряжения и QR" -ForegroundColor Green

    # B инициирует синк (A известен по строке сопряжения), ждём файл на B.
    $ok = $false
    for ($i = 0; $i -lt 60; $i++) {
        if (Test-Path (Join-Path $mB 'A\01 - First.flac')) { $ok = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ok) { throw 'файл не доехал до B' }
    Write-Host "OK: демоны синхронизировались по запомненному адресу" -ForegroundColor Green
} finally {
    Stop-Process -Id $pa.Id, $pb.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

# Низкий диск: порог 100% => всегда мало места.
$pl = Start-Daemon $dA '127.0.0.1:47904' '127.0.0.1:47905' @('--min-free-percent', '100')
try {
    $s = Invoke-RestMethod -Uri 'http://127.0.0.1:47905/api/status'
    if ($s.state -ne 'paused_with_warning') { throw "state при низком диске: $($s.state)" }
    if (-not $s.disk.low) { throw 'disk.low не выставлен' }
    $failed = $false
    try {
        Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:47905/api/sync' -ContentType 'application/json' -Body '{}' -TimeoutSec 20 | Out-Null
    } catch { $failed = $true }
    if (-not $failed) { throw 'синк не был отклонён при низком диске' }
    Write-Host "OK: низкий диск => paused_with_warning, синк отклонён с ошибкой" -ForegroundColor Green
} finally {
    Stop-Process -Id $pl.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

# Уведомление об удалении не должно ронять демон.
$pn = Start-Daemon $dA '127.0.0.1:47906' '127.0.0.1:47907' @()
try {
    Remove-Item (Join-Path $mA 'A\01 - First.flac') -Force
    $s = $null
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 500
        $s = Invoke-RestMethod -Uri 'http://127.0.0.1:47907/api/status'
        if ($s.pending.Count -ge 1) { break }
    }
    if ($s.pending.Count -lt 1) { throw 'удаление не попало в pending' }
    if ($pn.HasExited) { throw 'демон упал после уведомления' }
    Write-Host "OK: уведомление об удалении не роняет демон" -ForegroundColor Green
} finally {
    Stop-Process -Id $pn.Id -Force -ErrorAction SilentlyContinue
}

Write-Host "E2E демона пройден." -ForegroundColor Green
