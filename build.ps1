Write-Host "==============================" -ForegroundColor Cyan
Write-Host "    CHON PHIEN BAN BUILD" -ForegroundColor Cyan
Write-Host "==============================" -ForegroundColor Cyan
Write-Host " [1] Ban PUBLIC (Mac dinh)"
Write-Host " [2] Ban PERSONAL (Full-drive)"
Write-Host "------------------------------"

$choice = Read-Host "Nhap lua chon cua ban (1 hoac 2)"

if ($choice -eq '1') {
    Write-Host "Dang build ban PUBLIC..." -ForegroundColor Green
    cargo tauri build
} elseif ($choice -eq '2') {
    Write-Host "Dang build ban PERSONAL..." -ForegroundColor Green
    cargo tauri build --features full-drive --config src-tauri/tauri.personal.conf.json
} else {
    Write-Host "Lua chon khong hop le! Vui long chay lai va chon 1 hoac 2." -ForegroundColor Red
}