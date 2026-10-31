# Установка Android-toolchain для сборки SynctMusic (Tauri v2).
#
# Скрипт можно запускать повторно: уже скачанные архивы и установленные
# пакеты пропускаются. Всё складывается в один каталог, ничего не пишется
# в системные директории кроме пользовательских переменных окружения.
#
#   powershell -ExecutionPolicy Bypass -File scripts\android-setup.ps1
#
# После завершения откройте новый терминал (или вызовите android-env.ps1),
# чтобы JAVA_HOME / ANDROID_HOME / NDK_HOME попали в окружение.

param(
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'SyncthMusic\android')
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$downloads = Join-Path $InstallRoot 'downloads'
$jdkDir = Join-Path $InstallRoot 'jdk'
$gradleDir = Join-Path $InstallRoot 'gradle'
$sdkDir = Join-Path $InstallRoot 'sdk'
$ndkVersion = '27.2.12479018'
$ndkDir = Join-Path $sdkDir "ndk\$ndkVersion"

New-Item -ItemType Directory -Force $InstallRoot, $downloads | Out-Null
$logFile = Join-Path $InstallRoot 'setup.log'

function Log([string]$message) {
    $line = '[{0:HH:mm:ss}] {1}' -f (Get-Date), $message
    Write-Host $line
    Add-Content -Path $logFile -Value $line -Encoding UTF8
}

function Fetch([string]$url, [string]$dest) {
    if ((Test-Path $dest) -and ((Get-Item $dest).Length -gt 0)) {
        Log "уже скачано: $(Split-Path $dest -Leaf)"
        return
    }
    Log "скачивание: $url"
    $tmp = "$dest.part"
    # Большие архивы иногда обрываются: продолжаем с места остановки.
    for ($attempt = 1; $attempt -le 8; $attempt++) {
        & curl.exe -L --fail --retry 5 --retry-all-errors --retry-delay 3 `
            --connect-timeout 30 -C - -o $tmp $url
        if ($LASTEXITCODE -eq 0) {
            Move-Item -Force $tmp $dest
            Log "готово: $(Split-Path $dest -Leaf) ($([math]::Round((Get-Item $dest).Length / 1MB)) МБ)"
            return
        }
        $have = if (Test-Path $tmp) { [math]::Round((Get-Item $tmp).Length / 1MB) } else { 0 }
        Log "обрыв (curl $LASTEXITCODE, есть $have МБ) — повтор $attempt/8"
        Start-Sleep -Seconds 3
    }
    throw "не удалось скачать $url"
}

function Expand-Zip([string]$zip, [string]$dest) {
    New-Item -ItemType Directory -Force $dest | Out-Null
    Log "распаковка: $(Split-Path $zip -Leaf)"
    & tar.exe -xf $zip -C $dest
    if ($LASTEXITCODE -ne 0) {
        Log 'tar не справился, пробую Expand-Archive'
        Expand-Archive -Path $zip -DestinationPath $dest -Force
    }
}

function Set-UserEnv([string]$name, [string]$value) {
    [Environment]::SetEnvironmentVariable($name, $value, 'User')
    Set-Item -Path "Env:$name" -Value $value
}

# ---------------------------------------------------------------- JDK 21
$jdkZip = Join-Path $downloads 'temurin-jdk21.zip'
Fetch 'https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jdk/hotspot/normal/eclipse' $jdkZip

if (-not (Test-Path (Join-Path $jdkDir 'bin\java.exe'))) {
    if (Test-Path $jdkDir) { Remove-Item -Recurse -Force $jdkDir }
    $tmp = Join-Path $downloads 'jdk-extract'
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Zip $jdkZip $tmp
    $inner = Get-ChildItem $tmp -Directory | Select-Object -First 1
    Move-Item $inner.FullName $jdkDir
    Remove-Item -Recurse -Force $tmp
    Log "JDK распакован: $jdkDir"
}
Set-UserEnv 'JAVA_HOME' $jdkDir
$env:PATH = "$jdkDir\bin;$env:PATH"

# ---------------------------------------------------------------- Gradle
$gradleZip = Join-Path $downloads 'gradle-8.10.2-bin.zip'
Fetch 'https://services.gradle.org/distributions/gradle-8.10.2-bin.zip' $gradleZip

if (-not (Test-Path (Join-Path $gradleDir 'bin\gradle.bat'))) {
    if (Test-Path $gradleDir) { Remove-Item -Recurse -Force $gradleDir }
    $tmp = Join-Path $downloads 'gradle-extract'
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Zip $gradleZip $tmp
    $inner = Get-ChildItem $tmp -Directory | Select-Object -First 1
    Move-Item $inner.FullName $gradleDir
    Remove-Item -Recurse -Force $tmp
    Log "Gradle распакован: $gradleDir"
}
Set-UserEnv 'GRADLE_HOME' $gradleDir

# ---------------------------------------------------------------- Android SDK
$cmdlineZip = Join-Path $downloads 'commandlinetools.zip'
Fetch 'https://dl.google.com/android/repository/commandlinetools-win-13114758_latest.zip' $cmdlineZip

$sdkManager = Join-Path $sdkDir 'cmdline-tools\latest\bin\sdkmanager.bat'
if (-not (Test-Path $sdkManager)) {
    $tmp = Join-Path $downloads 'cmdline-extract'
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Expand-Zip $cmdlineZip $tmp
    $latest = Join-Path $sdkDir 'cmdline-tools\latest'
    if (Test-Path $latest) { Remove-Item -Recurse -Force $latest }
    New-Item -ItemType Directory -Force $latest | Out-Null
    $inner = Join-Path $tmp 'cmdline-tools'
    if (-not (Test-Path $inner)) { $inner = (Get-ChildItem $tmp -Directory | Select-Object -First 1).FullName }
    Copy-Item -Recurse -Force (Join-Path $inner '*') $latest
    Remove-Item -Recurse -Force $tmp
    Log "cmdline-tools установлены: $latest"
}

Set-UserEnv 'ANDROID_HOME' $sdkDir
Set-UserEnv 'ANDROID_SDK_ROOT' $sdkDir
$env:PATH = "$sdkDir\cmdline-tools\latest\bin;$sdkDir\platform-tools;$env:PATH"

# Лицензии SDK принимать нужно один раз; sdkmanager ждёт поток "y".
Log 'принятие лицензий Android SDK'
$answers = 1..60 | ForEach-Object { 'y' }
$answers | & $sdkManager --sdk_root="$sdkDir" --licenses | Out-Null

$packages = @(
    'platform-tools',
    'platforms;android-35',
    'build-tools;35.0.0',
    "ndk;$ndkVersion",
    'cmake;3.22.1'
)
Log 'установка пакетов SDK (platform-tools, android-35, build-tools 35, NDK)'
$answers | & $sdkManager --sdk_root="$sdkDir" $packages
if ($LASTEXITCODE -ne 0) { throw "sdkmanager завершился с кодом $LASTEXITCODE" }
if (-not (Test-Path $ndkDir)) { throw "NDK не найден: $ndkDir" }
Set-UserEnv 'NDK_HOME' $ndkDir

# ---------------------------------------------------------------- Rust targets
$targets = @(
    'aarch64-linux-android',
    'armv7-linux-androideabi',
    'i686-linux-android',
    'x86_64-linux-android'
)
$installed = & rustup target list --installed
foreach ($target in $targets) {
    if ($installed -contains $target) {
        Log "rust target уже есть: $target"
    } else {
        Log "rustup target add $target"
        & rustup target add $target
        if ($LASTEXITCODE -ne 0) { throw "rustup target add $target не удался" }
    }
}

# ---------------------------------------------------------------- PATH
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$need = @(
    (Join-Path $jdkDir 'bin'),
    (Join-Path $gradleDir 'bin'),
    (Join-Path $sdkDir 'cmdline-tools\latest\bin'),
    (Join-Path $sdkDir 'platform-tools'),
    $sdkDir
)
$parts = @()
if ($userPath) { $parts = $userPath -split ';' | Where-Object { $_ } }
foreach ($p in $need) {
    if (-not ($parts | Where-Object { $_.TrimEnd('\') -ieq $p.TrimEnd('\') })) { $parts += $p }
}
[Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')

# Скрипт для будущих терминалов: . "$env:LOCALAPPDATA\SyncthMusic\android\android-env.ps1"
$envScript = Join-Path $InstallRoot 'android-env.ps1'
@(
    '# Сгенерировано scripts\android-setup.ps1',
    "`$env:JAVA_HOME = '$jdkDir'",
    "`$env:GRADLE_HOME = '$gradleDir'",
    "`$env:ANDROID_HOME = '$sdkDir'",
    "`$env:ANDROID_SDK_ROOT = '$sdkDir'",
    "`$env:NDK_HOME = '$ndkDir'",
    "`$env:PATH = `"$jdkDir\bin;$gradleDir\bin;$sdkDir\cmdline-tools\latest\bin;$sdkDir\platform-tools;$sdkDir;`$env:PATH`""
) | Set-Content -Path $envScript -Encoding UTF8

Log '--- всё установлено ---'
Log "JDK:    $jdkDir"
Log "Gradle: $gradleDir"
Log "SDK:    $sdkDir"
Log "NDK:    $ndkDir"
Log "Проверка: java -version; gradle --version; sdkmanager --list_installed"
