# Подписывает KbBatteryTray.exe закрытым ключом релизов -> KbBatteryTray.exe.sig (ECDSA P-256 + SHA-256).
# Виджет ставит обновление, только если подпись сходится с открытым ключом release.pub, вшитым в exe.
# Первый раз (или при смене ключа): .\scripts\sign-release.ps1 -NewKey — создаст ключ и release.pub,
# после этого exe нужно пересобрать, чтобы в него попал новый release.pub.
param([switch]$NewKey)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Core
$root = Split-Path $PSScriptRoot
$keyFile = Join-Path $env:USERPROFILE '.kbbatterytray\release.key'  # закрытый ключ: только здесь, не в репо

if ($NewKey) {
    if (Test-Path $keyFile) { throw "Ключ уже есть: $keyFile. Старые версии примут обновления только с ним — не перезаписываю." }
    $p = New-Object Security.Cryptography.CngKeyCreationParameters
    $p.ExportPolicy = [Security.Cryptography.CngExportPolicies]::AllowPlaintextExport
    $k = [Security.Cryptography.CngKey]::Create([Security.Cryptography.CngAlgorithm]::ECDsaP256, [NullString]::Value, $p)  # без имени — ключ только в памяти, не в хранилище Windows
    New-Item -ItemType Directory -Force (Split-Path $keyFile) | Out-Null
    [IO.File]::WriteAllBytes($keyFile, $k.Export([Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob))
    [IO.File]::WriteAllBytes((Join-Path $root 'release.pub'), $k.Export([Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob))
    "Новый ключ: $keyFile (сохрани копию!), открытый — release.pub. Пересобери exe."
    return
}

if (-not (Test-Path $keyFile)) { throw "Нет ключа $keyFile. Без него обновления не подписать." }
$key = [Security.Cryptography.CngKey]::Import([IO.File]::ReadAllBytes($keyFile), [Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
$ecdsa = New-Object Security.Cryptography.ECDsaCng($key)
$exe = Join-Path $root 'KbBatteryTray.exe'
$data = [IO.File]::ReadAllBytes($exe)
$sig = $ecdsa.SignData($data)  # SHA-256, подпись r||s 64 байта — тот формат, что ждёт BCryptVerifySignature

# Сверяем, что подпись подходит к release.pub, вшитому в этот exe
$pub = [Security.Cryptography.CngKey]::Import([IO.File]::ReadAllBytes((Join-Path $root 'release.pub')), [Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
if (-not (New-Object Security.Cryptography.ECDsaCng($pub)).VerifyData($data, $sig)) { throw 'Подпись не сходится с release.pub' }
[IO.File]::WriteAllBytes("$exe.sig", $sig)
"Подписано: $exe.sig"
