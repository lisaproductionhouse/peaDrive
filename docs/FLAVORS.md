# Hai bản build (flavor) từ một mã nguồn

peaDrive có hai bản build. **Mã nghiệp vụ giống hệt nhau**: quyền của từng phiên đăng nhập được suy ra lúc chạy từ trường `scope` Google trả về (`auth::Grants`), nên không có `#[cfg]` nào rẽ nhánh theo bản build trong logic tải / tải lên / sửa / xoá.

| | **public** (mặc định) | **personal** (`full-drive`) |
|---|---|---|
| Dành cho | phát hành rộng rãi (phải qua xác minh Google) | dùng cá nhân, không phát hành |
| Scope xin lúc đăng nhập | `drive.readonly` + `drive.file` | `drive` (toàn quyền) |
| Ghi được vào | chỉ mục do app tạo | mọi file |
| Tên app · `identifier` | `peaDrive` · `app.peadrive.desktop` | `peaDrive Personal` · `app.peadrive.personal` |
| Service keychain | `peaDrive` | `peaDrive-personal` |
| OAuth client (khoá cứng lúc build) | `.env.public` — project Google Cloud public | `.env.personal` — **project riêng** |
| Build | `cargo tauri build` | `cargo tauri build --features full-drive --config src-tauri/tauri.personal.conf.json` |
| Test | `cargo test -p peadrive-core` | `cargo test -p peadrive-core --features full-drive` |

Cả hai bản dùng chung một phiên bản — hiện `0.0.1` (giai đoạn đang phát triển); xem mục "Phiên bản" trong README.

## Chỉ những thứ sau được khác nhau giữa hai bản

1. **Bộ scope** — hằng `OAUTH_SCOPES` trong `crates/peadrive-core/src/config.rs`, chọn bằng cờ Cargo `full-drive` (khai ở `crates/peadrive-core/Cargo.toml`, chuyển tiếp ở `src-tauri/Cargo.toml`).
2. **Danh tính**, để cài song song trên một máy mà không đọc nhầm dữ liệu / token của nhau: `productName` + `identifier` trong `src-tauri/tauri.personal.conf.json` (gộp vào `tauri.conf.json` bằng `--config`) và hằng `KEYCHAIN_SERVICE` trong `crates/peadrive-core/src/auth/store.rs`.
3. **OAuth client** — file `.env.<flavor>` ở gốc dự án (mục dưới).
4. **Vài assert test** phụ thuộc scope (chuỗi scope trong URL đăng nhập, `Grants` mặc định, `can_modify` của shortcut) dùng `cfg!(feature = "full-drive")`, nên cùng một bộ test chạy cho cả hai bản và mỗi bản bị khoá đúng bộ scope của mình.
5. **Tài liệu xác minh** (`docs/GOOGLE_VERIFICATION.md`) chỉ áp dụng cho bản public.

Muốn thêm tính năng: viết một lần, chạy test cả hai bản. Nếu thấy cần `#[cfg(feature = "full-drive")]` trong logic nghiệp vụ, hãy dùng `Grants` thay thế.

## OAuth client: khoá cứng bằng `.env.public` / `.env.personal`

- Mỗi flavor một file ở **gốc dự án**: `.env.public` (mặc định) và `.env.personal` (cờ `full-drive`). Sao chép từ `.env.public.example` / `.env.personal.example` rồi điền `PEADRIVE_CLIENT_ID` và `PEADRIVE_CLIENT_SECRET`. File thật chứa secret nên nằm trong `.gitignore`; chỉ commit file `.example`.
- `crates/peadrive-core/build.rs` chọn file bằng đúng cờ `full-drive` đang quyết định bộ scope, rồi nhúng giá trị vào app (`option_env!` trong `OAuthConfig::builtin()`). Vì vậy không thể nhúng nhầm client của flavor kia.
- **File này là nguồn duy nhất.** `PEADRIVE_CLIENT_*` trong biến môi trường của shell bị bỏ qua (biến còn sót từ flavor kia không lọt vào bản build). Lúc chạy app không đọc client từ file (`oauth.json` cũ nếu còn thì bị bỏ qua), biến môi trường hay Cài đặt; hộp thoại Cài đặt không có ô nhập API.
- File thiếu hoặc chưa điền đủ cả hai khoá: build **phát hành** (`cargo tauri build`, profile release) dừng với lỗi rõ ràng; build dev / test chỉ cảnh báo và đăng nhập báo "chưa được nhúng Client ID". Khi file chưa tồn tại, cargo chạy lại `build.rs` ở mỗi lần build (chủ ý: tạo file xong là có hiệu lực ngay, không cần `cargo clean`).
- Bộ đọc hỗ trợ `export `, nháy đơn / kép, chú thích `#`, CRLF và BOM (Notepad / PowerShell hay thêm) và có unit test (`crates/peadrive-core/build_env.rs`).

## Build trên máy

```bash
cp .env.public.example .env.public         # rồi điền Client ID / secret của project public
cargo tauri build                          # public

cp .env.personal.example .env.personal     # rồi điền Client ID / secret của project cá nhân
cargo tauri build --features full-drive --config src-tauri/tauri.personal.conf.json   # personal
```

## CI (`.github/workflows/build.yml`)

Test chạy cho cả hai flavor (không cần file `.env`); bundle Windows + macOS cho cả hai flavor. Job bundle ghi secret của flavor vào `.env.<flavor>` rồi mới build, và **dừng với lỗi rõ ràng** nếu thiếu secret (không lặng lẽ dùng nhầm client của flavor kia). Cần bốn secret của repo:

| Secret | Dùng cho |
|---|---|
| `PEADRIVE_CLIENT_ID`, `PEADRIVE_CLIENT_SECRET` | bản public (`.env.public`) |
| `PEADRIVE_PERSONAL_CLIENT_ID`, `PEADRIVE_PERSONAL_CLIENT_SECRET` | bản personal (`.env.personal`) |

## Lưu ý

- **Mỗi bản một project Google Cloud.** Scope được khai ở *OAuth consent screen → Data access* theo từng project và phải khớp `OAUTH_SCOPES` từng chữ. Project của bản personal (scope `drive`) để ở chế độ *Testing* với tài khoản của bạn làm Test user (refresh token sống 7 ngày) hoặc *Internal* nếu dùng Google Workspace; không nộp xác minh.
- **Dữ liệu cũ của bản cá nhân.** Bản `full-drive` dùng `identifier` và service keychain riêng, nên lần đầu cần đăng nhập lại và cài đặt / hàng đợi bắt đầu mới. Muốn giữ danh tính cũ (`app.peadrive.desktop` / `peaDrive`) thì sửa hai chỗ ở mục 2 — đổi lại là hai bản dùng chung thư mục dữ liệu và token khi cài cùng một máy.
- **Kiểm tra phiên bản dùng chung.** Cả hai bản gọi máy chủ phiên bản với cùng slug (`APP_SLUG`) và nút *Cập nhật ngay* mở liên kết tải của bản public. Bản personal nên luôn được build lại cùng lúc với bản public để không rơi vào lớp chặn bắt buộc cập nhật.
