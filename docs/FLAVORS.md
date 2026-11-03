# Hai bản build (flavor) từ một mã nguồn

peaDrive có hai bản build. **Mã nghiệp vụ giống hệt nhau**: quyền của từng phiên đăng nhập được suy ra lúc chạy từ trường `scope` Google trả về (`auth::Grants`), nên không có `#[cfg]` nào rẽ nhánh theo bản build trong logic tải / tải lên / sửa / xoá.

| | **public** (mặc định) | **personal** (`full-drive`) |
|---|---|---|
| Dành cho | phát hành rộng rãi (phải qua xác minh Google) | dùng cá nhân, không phát hành |
| Scope xin lúc đăng nhập | `drive.readonly` + `drive.file` | `drive` (toàn quyền) |
| Ghi được vào | chỉ mục do app tạo | mọi file |
| Tên app · `identifier` | `peaDrive` · `app.peadrive.desktop` | `peaDrive Personal` · `app.peadrive.personal` |
| Service keychain | `peaDrive` | `peaDrive-personal` |
| OAuth client (project Google Cloud) | project public | **project riêng** |
| Build | `cargo tauri build` | `cargo tauri build --features full-drive --config src-tauri/tauri.personal.conf.json` |
| Test | `cargo test -p peadrive-core` | `cargo test -p peadrive-core --features full-drive` |

## Chỉ những thứ sau được khác nhau giữa hai bản

1. **Bộ scope** — hằng `OAUTH_SCOPES` trong `crates/peadrive-core/src/config.rs`, chọn bằng cờ Cargo `full-drive` (khai ở `crates/peadrive-core/Cargo.toml`, chuyển tiếp ở `src-tauri/Cargo.toml`).
2. **Danh tính**, để cài song song trên một máy mà không đọc nhầm dữ liệu / token của nhau: `productName` + `identifier` trong `src-tauri/tauri.personal.conf.json` (gộp vào `tauri.conf.json` bằng `--config`) và hằng `KEYCHAIN_SERVICE` trong `crates/peadrive-core/src/auth/store.rs`.
3. **OAuth client** — Client ID / secret nhúng lúc build qua biến môi trường (không nằm trong mã nguồn).
4. **Vài assert test** phụ thuộc scope (chuỗi scope trong URL đăng nhập, `Grants` mặc định, `can_modify` của shortcut) dùng `cfg!(feature = "full-drive")`, nên cùng một bộ test chạy cho cả hai bản và mỗi bản bị khoá đúng bộ scope của mình.
5. **Tài liệu xác minh** (`docs/GOOGLE_VERIFICATION.md`) chỉ áp dụng cho bản public.

Muốn thêm tính năng: viết một lần, chạy test cả hai bản. Nếu thấy cần `#[cfg(feature = "full-drive")]` trong logic nghiệp vụ, hãy dùng `Grants` thay thế.

## Build trên máy

```bash
# OAuth client của đúng bản bạn build (mỗi bản một project Google Cloud)
export PEADRIVE_CLIENT_ID=xxxx.apps.googleusercontent.com
export PEADRIVE_CLIENT_SECRET=xxxx

cargo tauri build                                                                                  # public
cargo tauri build --features full-drive --config src-tauri/tauri.personal.conf.json                # personal
```

Build không nhúng Client ID (dev) vẫn chạy: hộp thoại Cài đặt hiện ô nhập Client ID / secret. Build đã nhúng thì hộp thoại không có ô này.

## CI (`.github/workflows/build.yml`)

Test chạy cho cả hai flavor; bundle Windows + macOS cho cả hai flavor. Cần bốn secret của repo — thiếu secret của flavor nào thì job bundle của flavor đó **dừng với lỗi rõ ràng** (không lặng lẽ dùng nhầm client của flavor kia):

| Secret | Dùng cho |
|---|---|
| `PEADRIVE_CLIENT_ID`, `PEADRIVE_CLIENT_SECRET` | bản public |
| `PEADRIVE_PERSONAL_CLIENT_ID`, `PEADRIVE_PERSONAL_CLIENT_SECRET` | bản personal |

## Lưu ý

- **Mỗi bản một project Google Cloud.** Scope được khai ở *OAuth consent screen → Data access* theo từng project và phải khớp `OAUTH_SCOPES` từng chữ. Project của bản personal (scope `drive`) để ở chế độ *Testing* với tài khoản của bạn làm Test user (refresh token sống 7 ngày) hoặc *Internal* nếu dùng Google Workspace; không nộp xác minh.
- **Dữ liệu cũ của bản cá nhân.** Bản `full-drive` dùng `identifier` và service keychain riêng, nên lần đầu cần đăng nhập lại và cài đặt / hàng đợi bắt đầu mới. Muốn giữ danh tính cũ (`app.peadrive.desktop` / `peaDrive`) thì sửa hai chỗ ở mục 2 — đổi lại là hai bản dùng chung thư mục dữ liệu và token khi cài cùng một máy.
- **Kiểm tra phiên bản dùng chung.** Cả hai bản gọi máy chủ phiên bản với cùng slug (`APP_SLUG`) và nút *Cập nhật ngay* mở liên kết tải của bản public. Bản personal nên luôn được build lại cùng lúc với bản public để không rơi vào lớp chặn bắt buộc cập nhật.
