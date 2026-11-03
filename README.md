# peaDrive

Ứng dụng desktop **Windows & macOS** tải file/thư mục Google Drive tốc độ cao. **100% Rust + Tauri 2** — không Go, không server giao diện trên `127.0.0.1`. Giao diện nằm ở `ui/` (`index.html` + `style.css` + `app.js`); icon ứng dụng là `logo_peamark_icon.png` (bộ icon trong `src-tauri/icons/`, tạo lại bằng `cargo tauri icon <png>`).

- **Xác thực:** Google OAuth 2.0 **PKCE (S256)**. Backend Rust mở *trình duyệt mặc định của OS*, nhận `code`, đổi token, lưu trong Keychain (macOS) / Credential Manager (Windows).
- **Tải:** Direct Chunked Streaming — mỗi file chia chunk, **worker pool** tải song song bằng `Range`, ghi qua **ring buffer** thẳng xuống `.part` bằng `pwrite` đúng offset.
- **Quota:** dùng API chính thức bằng tài khoản của bạn; tự giảm worker khi bị rate limit; khi file báo `downloadQuotaExceeded` thì copy server-side vào Drive cá nhân, tải bản copy rồi xoá (với scope `drive.file`, bước copy chỉ chạy được khi Drive cho phép copy file nguồn; nếu bị từ chối, app báo cách tự tạo bản sao trên Drive rồi dán link bản sao).
- **Resume:** `<tên>.part` + `<tên>.peadrive` (bitmap chunk) · khôi phục hàng đợi sau khi mở lại app.
- **Tải lên thủ công:** chọn file/thư mục bên khung *Máy của tôi* rồi bấm **Tải lên** (hoặc kéo-thả sang khung Google Drive): app dựng lại cấu trúc thư mục trên Drive, xếp mỗi file vào **Hàng đợi** (phiên *resumable*) và bỏ qua file đã có đúng phiên bản. Xem mục [Tải lên thủ công](#tải-lên-thủ-công).
- **Smart Upload:** worker nền tự quét thư mục render (DaVinci, Capture One…) và tải file mới lên Drive qua phiên *resumable*; chỉ tải file đã **không bị sửa đổi ít nhất 2 phút**. Xem mục [Smart Upload](#smart-upload-tải-lên-tự-động-file-đang-render).

Kiến trúc chi tiết (kèm bảng endpoint giao diện ↔ backend): [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

**Hai bản build từ một mã nguồn:** bản mặc định (*public*, phát hành rộng rãi) xin `drive.readonly` + `drive.file`; bản cá nhân (*personal*, cờ Cargo `full-drive`) xin toàn quyền `drive`. Hai bản chỉ khác nhau ở bộ scope và danh tính (tên app, thư mục dữ liệu, keychain, OAuth client) — xem [`docs/FLAVORS.md`](docs/FLAVORS.md).

## Chuẩn bị Google OAuth (một lần)

1. [Google Cloud Console](https://console.cloud.google.com/) → tạo project → bật **Google Drive API**.
2. *OAuth consent screen* → thêm tài khoản của bạn làm **Test user**; ở mục *Data access* khai báo **đúng hai scope** `drive.readonly` và `drive.file` (xem bên dưới; bản `full-drive`: đúng **một** scope `drive`, ở project riêng).
3. *Credentials* → **Create credentials → OAuth client ID** → loại **Desktop app**.
4. Mở peaDrive và bấm đăng nhập: nếu bản build chưa nhúng sẵn Client ID, hộp thoại **Cài đặt** tự mở (cũng mở được bằng **Ctrl/⌘ + ,** hoặc nút bánh răng) — dán *Client ID* và *Client secret*. Bản build đã nhúng sẵn (đặt `PEADRIVE_CLIENT_ID` / `PEADRIVE_CLIENT_SECRET` lúc build) thì bỏ qua bước này và hộp thoại không có ô nhập OAuth.

> **Lưu ý**
> - **Scope** (hằng `OAUTH_SCOPES` trong `crates/peadrive-core/src/config.rs`; phải khớp từng chữ với danh sách ở consent screen). Bản mặc định xin hai scope:
>   - `https://www.googleapis.com/auth/drive.readonly` — xem và tải mọi file (duyệt, liệt kê, tải về, export). Google xếp vào nhóm **restricted**.
>   - `https://www.googleapis.com/auth/drive.file` — tạo file/thư mục mới; sửa/xoá/chia sẻ **chỉ những mục do chính peaDrive tạo**. Nhóm non-sensitive.
>   - Hệ quả: peaDrive **không** đổi tên/xoá/chia sẻ được file có sẵn của bạn trên Drive (các mục đó bị làm mờ trong menu chuột phải) và chỉ tải lên được vào *Drive của tôi* hoặc thư mục do peaDrive tạo. Nếu thư mục cùng tên đã có sẵn, peaDrive tạo thư mục `<tên> (peaDrive)` bên cạnh. Chi tiết: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#quyền-drive-scope).
>   - Vì có `drive.readonly`, phát hành rộng rãi vẫn phải qua *restricted scope verification* của Google. Hướng dẫn nộp hồ sơ: [`docs/GOOGLE_VERIFICATION.md`](docs/GOOGLE_VERIFICATION.md).
>   - Bản `full-drive` (cá nhân) xin **một** scope `https://www.googleapis.com/auth/drive` (restricted): toàn quyền nên không có các hạn chế trên. Dùng project / Client ID riêng và không phát hành rộng rãi (xem [`docs/FLAVORS.md`](docs/FLAVORS.md)).
>   - **Thông báo quyền riêng tư** kèm link <https://peamark.com/privacy> hiển thị ngay trong giao diện: màn hình đăng nhập (ngay dưới nút *Kết nối Google Drive*) và hộp thoại *Giới thiệu* (bấm vào số phiên bản ở chân thanh bên); hằng `PRIVACY_URL` trong `ui/app.js`.
> - Project ở trạng thái **Testing** → refresh token chỉ sống **7 ngày**. Chuyển consent screen sang **In production** để không phải đăng nhập lại.
> - `client_secret` của Desktop app không phải bí mật thật sự nhưng Google vẫn yêu cầu khi đổi code.
> - Chỉ có **một** socket loopback tạm trên `127.0.0.1:<cổng ngẫu nhiên>` trong lúc đăng nhập (Google bắt buộc với client Desktop), đóng ngay khi nhận code. Giao diện chạy trong WebView của Tauri, nạp qua `tauri://`.

## Chạy / build

Yêu cầu: Rust ≥ 1.85, [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) (Windows: WebView2 + MSVC; macOS: Xcode CLT).

```bash
cargo install tauri-cli --version "^2" --locked

# (tuỳ chọn) nhúng sẵn OAuth client lúc build
export PEADRIVE_CLIENT_ID=xxxx.apps.googleusercontent.com
export PEADRIVE_CLIENT_SECRET=xxxx

cargo tauri dev            # chạy thử (bản public)
cargo tauri build          # .msi/.exe (Windows), .app/.dmg (macOS)

# Bản cá nhân (toàn quyền Drive): scope `drive`, tên app / identifier / keychain riêng — xem docs/FLAVORS.md
cargo tauri build --features full-drive --config src-tauri/tauri.personal.conf.json
```

Kiểm thử (không cần Tauri/WebView):

```bash
cargo test -p peadrive-core                          # unit + e2e với Drive giả lập (Range, ngắt kết nối, 429, quota, API của giao diện)
cargo test -p peadrive-core --features full-drive     # cùng bộ test cho bản cá nhân (assert scope tự đổi theo bản build)
cd tools/ui-smoke && npm install && npm test         # index.html + bridge.js trong jsdom với JSON thật do Rust sinh ra
```

## Cài đặt hiệu năng (Ctrl/⌘ + ,)

| Mục | Mặc định | Ghi chú |
|---|---|---|
| Worker mỗi file | 8 | tối đa 32; tự giảm một nửa khi rate limit, tăng lại 1 mỗi 10s yên ổn |
| Chunk | 16 MiB | 1–128 MiB |
| Ring buffer mỗi worker | 8 MiB | RAM tối đa ≈ worker × ring × số file song song |
| File song song | 2 | |

## Cấu trúc

```
crates/peadrive-core   lõi: auth (đa tài khoản) · drive (client + pool theo tài khoản) · local · update (kiểm tra phiên bản) · api (định tuyến /api/*) · engine (ring, writer, worker, job, manager)
src-tauri              vỏ Tauri: lệnh `api`, cửa sổ + bridge.js, tauri.conf.json
ui/                    index.html + style.css + app.js (giao diện) + qrtpbank.svg
tools/ui-smoke         thử giao diện trong jsdom
```

## Giao diện (`ui/`)

`index.html` nạp `style.css` và `app.js` (cùng thư mục); `qrtpbank.svg` nằm cạnh và được nhúng vào app. Lần cập nhật đa tài khoản / Link Mode **có sửa `index.html`** (đa tài khoản và Link Mode cần logic trong trang): xem chính xác các chỗ đổi ở [`docs/index.html.changes.patch`](docs/index.html.changes.patch) để gộp vào bản của bạn.

| Tính năng | Cách hoạt động |
|---|---|
| **Đa tài khoản, chuyển tức thì** | Mỗi tài khoản Google có token riêng trong Keychain / Credential Manager; danh sách ở `accounts.json`. Menu tài khoản lấy danh sách từ backend; **chuyển tài khoản = một lệnh nội bộ** (không mở trình duyệt, không đăng nhập lại). "Thêm tài khoản khác" mở đăng nhập Google (luôn có hộp chọn tài khoản). Tác vụ gắn với tài khoản đã tạo ra nó nên vẫn chạy đúng khi bạn đổi tài khoản; tài khoản mất phiên (token bị thu hồi/hết hạn) thì giao diện tự mở đăng nhập lại cho riêng tài khoản đó. |
| **Link Mode** | Chọn *Link Drive* ở khung Google Drive → dán link → bấm truy cập: **cả khung Drive chuyển sang nội dung của link** (file hoặc thư mục, kể cả link cũ có `resourcekey`), tách hẳn khỏi Drive của tôi, chỉ xem/tải (không tải lên, tạo thư mục, đổi tên, xoá, chia sẻ). Không còn popup nhập link / chọn thư mục đích: chọn mục rồi bấm Tải xuống hoặc kéo sang khung máy. Chọn lại *Drive của tôi* hoặc bất kỳ mục thanh bên nào để thoát, trạng thái cũ được khôi phục. Link sai/không truy cập được thì báo lỗi và ở nguyên chế độ cũ. Dán link (Ctrl+V) hoặc Ctrl+L chỉ điền sẵn ô link; thả link vào khung thì truy cập luôn. |
| **Kiểm tra phiên bản lúc khởi động** | Xem mục dưới. |
| Đa ngôn ngữ VI / EN / ZH | do trang tự dịch (`peadrive.lang`); hộp thoại **Cài đặt** và lớp chặn phiên bản của cầu nối cũng đổi theo ngôn ngữ |
| Thanh dung lượng Drive, Thông tin ứng dụng | `/api/drive/about`, `/api/update/check` |
| **Nhập danh sách (.txt)** | Nút ở đầu mỗi khung: mỗi dòng là một tên file/thư mục (có thể kèm đường dẫn; dòng trống và dòng bắt đầu bằng `#` bị bỏ qua), khớp không phân biệt hoa/thường; dòng không có đuôi (vd `DSC00123`) khớp mọi file cùng tên gốc (`.ARW`, `.JPG`…). Các mục khớp được tick (thay cho vùng chọn hiện tại), chỉ đối chiếu với mục đang hiển thị. |
| **Đảo chiều vùng chọn** | Nút icon bên trái tiêu đề cột *Tên*: chưa chọn mục nào → **chọn tất cả**; đã có mục được chọn → **đảo chiều**. |
| **Thanh công cụ** | *Thư mục mới* và *Tải xuống* nằm cạnh menu tài khoản; *Smart Upload* nằm bên trái *Tải lên*. |
| **Nút Cài đặt** | Biểu tượng bánh răng ở cuối thanh công cụ (sau nút sáng/tối) mở bảng cài đặt bất cứ lúc nào; Ctrl/⌘ + `,` vẫn dùng được. Bản build đã nhúng sẵn OAuth client thì bảng chỉ còn thiết lập cá nhân (hiệu năng tải, quota & toàn vẹn), không có ô nhập API/OAuth; bản chưa nhúng (dev) vẫn hiện ô nhập Client ID / secret. |
| **Chấm trạng thái** (thanh dưới) | Thay chữ "Sẵn sàng": **xanh** = sẵn sàng, **đỏ** = chưa sẵn sàng, tooltip nêu lý do — chưa đăng nhập Google Drive, không liên lạc được với Google (mất mạng), hoặc không liên lạc được với backend. Dữ liệu thật: backend trả `ready` + `ready_reason` trong `/api/transfer/queue` (hỏi mỗi 0,5 s) và `/api/status` (mỗi 15 s); `DriveClient::reachable` dựa vào kết quả các request thật trong 10 giây gần nhất, hoặc một `HEAD` nhẹ khi đang rảnh. |
| **Smart Upload** | Xem mục dưới. |

## Tải lên thủ công

Chọn file và/hoặc thư mục bên khung **Máy của tôi**, mở thư mục Drive đích bên khung Google Drive rồi bấm **Tải lên** (hoặc kéo-thả sang khung Drive, hoặc chuột phải → *Tải lên Google Drive*).

- **Cấu trúc đích:** thư mục đã chọn trở thành thư mục con của thư mục Drive đang mở, giữ nguyên thư mục con (kể cả thư mục rỗng); file chọn lẻ vào thẳng thư mục đang mở. Bỏ qua file ẩn (tên bắt đầu bằng `.`), liên kết tượng trưng và file rỗng (Drive không nhận phiên 0 byte); quét tối đa 16 cấp thư mục con.
- **Không tải trùng:** mỗi file được đối chiếu với Drive bằng cùng cơ chế chữ ký `pdSrc` như Smart Upload: đã có đúng phiên bản thì bỏ qua; file do peaDrive tải lên trước đó mà nguồn đã đổi thì ghi đè đúng file cũ; còn lại tạo mới. Khác Smart Upload: chạy **một lần**, mọi định dạng và không bắt file phải "yên" 2 phút (bạn tự chọn file đã xong).
- **Quyền `drive.file` (bản mặc định):** chỉ ghi được vào *Drive của tôi* và thư mục do peaDrive tạo. Nếu thư mục đang mở **không** do peaDrive tạo, app **hỏi** trước rồi mới tải vào `Drive của tôi › peaDrive Uploads › <tên thư mục đang mở>` (thư mục do app tạo; sau đó bạn kéo sang đúng chỗ trên Google Drive nếu muốn). File cùng tên do bạn tự tải lên bằng web không bao giờ bị ghi đè. Bản `full-drive` ghi được vào mọi thư mục nên không hỏi.
- Mỗi file là một tác vụ trong **Hàng đợi** (tạm dừng / huỷ / thử lại như tải xuống). Tác vụ dở dang không được lưu qua lần mở app sau; chạy lại đúng thao tác đó thì file đã lên được bỏ qua.

API: `POST /api/transfer/upload` (body `{ folder_id, paths, use_fallback? }`) → `{ queued, skipped, folders, destination, warnings }`. Nếu đích không ghi được và chưa có `use_fallback`, trả `{ needs_fallback: true, blocked_name, destination }` mà **chưa làm gì**.

## Smart Upload (tải lên tự động file đang render)

Chọn một (hoặc nhiều) thư mục bên khung **Máy của tôi**, mở thư mục Drive đích bên khung Google Drive, bấm **Smart Upload** → chọn định dạng (Video: MP4, MOV, MXF, AVI; Ảnh: ARW, CR2, CR3, NEF, DNG, JPG/JPEG, PNG — tick sẵn — và ô nhập thêm đuôi file tuỳ ý) → **Bắt đầu tải lên**. Nút chỉ bấm được khi đang chọn ít nhất một thư mục. Mỗi thư mục có một worker nền ở backend (`engine/smart.rs`) quét thư mục (cả thư mục con) mỗi 10 giây và tự tải lên file mới do DaVinci Resolve, Capture One… xuất ra.

- **Luật bắt buộc — không tải file đang render dở:** chỉ đưa vào hàng đợi file có `Modified Time` cách giờ hệ thống **ít nhất 2 phút** (`SMART_MIN_AGE`). Điều kiện này được kiểm tra lại lần nữa ngay lúc bắt đầu gửi (tác vụ có thể chờ lâu trong hàng đợi), và nếu file bị ghi tiếp giữa chừng thì việc gửi dừng lại. File bị ghi lại phải "yên" đủ 2 phút một lần nữa. File rỗng hoặc đang bị phần mềm khác khoá độc quyền cũng được chờ.
- **Đích:** `<thư mục Drive đang mở>/<tên thư mục nguồn>/<thư mục con…>/<file>`; thư mục còn thiếu được tạo tự động. Hộp thoại hiện sẵn đường dẫn đích.
- **Không tải trùng:** trước khi xếp hàng, worker đối chiếu với Drive. Chưa có → tạo mới. Đã có đúng phiên bản này (chữ ký `dung lượng:ngày sửa` ghi trong `appProperties.pdSrc`), hoặc file tải tay cùng tên và cùng dung lượng → bỏ qua. Có bản cũ của chính file này → **ghi đè** (giữ link; Drive giữ lịch sử phiên bản), nên render lại cùng tên không sinh bản trùng. Bước đối chiếu này được làm hai lần — lúc quét (để không xếp hàng file đã có) và lại ngay lúc tác vụ bắt đầu gửi — nên dù file bị ghi lại giữa chừng hay tác vụ lỗi được xếp hàng lại cũng không thể sinh bản trùng; file đã có đúng phiên bản hiện là dòng *Đã có sẵn* trong hàng đợi.
- **Tải lên:** phiên *resumable* của Drive, chunk theo cài đặt Chunk nhưng tối đa 32 MiB, tiếp tục được sau mất mạng / tạm dừng / lỗi 5xx. Tác vụ hiện trong **Hàng đợi** (cột Thao tác = Tải lên) và tạm dừng / huỷ / thử lại như tải xuống; tác vụ lỗi tự xếp hàng lại sau 5 phút. "Tạm dừng tất cả" cũng dừng quét.
- **Quản lý:** khi có thư mục đang được theo dõi, thanh Hàng đợi hiện nút ⚡ kèm số thư mục → danh sách (đã tải lên / đã có trên Drive / đang chờ ổn định) và nút **Dừng**. Dừng chỉ ngừng theo dõi; file đã vào hàng đợi vẫn tải tiếp.

API: `POST /api/transfer/smart-upload` (body `{ folder_id, paths, extensions }`), `GET /api/transfer/smart-upload`, `POST /api/transfer/smart-upload/stop?id=`; `GET /api/transfer/queue` có thêm trường `smart`.

## Kiểm tra phiên bản (máy chủ của bạn)

Ngay khi app khởi động, backend gọi `GET <API>?app=peadrive&version=<phiên bản app>` (đúng `check_version.php` + `app_config.json` bạn gửi):

- `force_update = true` (thấp hơn `min_required_version`) → **app bị chặn hoàn toàn**: lớp phủ làm mờ toàn bộ app, bên trên là modal không tắt/ẩn được ("Phiên bản ứng dụng của bạn đã cũ. Vui lòng cập nhật lên phiên bản mới nhất để tiếp tục sử dụng." + nút **Cập nhật ngay**). Nút mở **liên kết tải thẳng** `https://www.peamark.com/app-api/download.php?app=peadrive&os=win` bằng trình duyệt mặc định. Việc chặn nằm **ở backend**: mọi lời gọi khác bị từ chối và các tác vụ đang chạy bị tạm dừng, nên không vượt qua được bằng cách bỏ qua giao diện.
- `update_available = true` (thấp hơn `latest_version`) → chỉ hiện thông báo có bản mới như trước, không chặn.
- Không liên lạc được máy chủ / máy chủ trả lỗi → **không chặn** (chỉ chặn khi máy chủ nói rõ phiên bản đã cũ).
- Phiên bản app lấy từ `version` trong `Cargo.toml` (workspace) và `src-tauri/tauri.conf.json` — **hiện đặt `0.0.1`** (khớp `latest_version` trong `app_config.json`); nhớ đổi cả hai khi phát hành bản mới.
- URL API mặc định `https://www.peamark.com/app-api/check_version.php`; cố định trong mã nguồn (`config.rs`). Muốn trỏ một bản build sang máy chủ khác (thử nghiệm) thì đặt biến môi trường `PEADRIVE_VERSION_API` **lúc build**; lúc chạy app không đọc biến môi trường nên người dùng cuối không đổi được.
- **Liên kết tải bản mới** — nút *Cập nhật ngay* của lớp chặn và nút *Tải về* của thanh "Đã có bản mới" — là hằng số `UPDATE_URL` trong `crates/peadrive-core/src/config.rs`: `https://www.peamark.com/app-api/download.php?app=peadrive&os=win` trên Windows, `os=mac` trên macOS (`https://www.peamark.com/app-api/download.php?app=peadrive&os=mac`). Địa chỉ phải là `https` (route mở link từ chối mọi scheme khác) và chỉ khai báo ở đúng một chỗ: `bridge.js` không giữ bản sao nào mà hỏi backend qua `/api/status`.

## Giới hạn đã biết

- Tải xuống, tải lên (thủ công và Smart Upload), duyệt Drive (Của tôi, Được chia sẻ, Có gắn dấu sao, Thùng rác, Bộ nhớ), tạo thư mục, đổi tên, xoá, chia sẻ link đều hoạt động; riêng tải lên và các thao tác sửa/xoá/chia sẻ bị giới hạn theo quyền `drive.file` ở bản mặc định (xem mục *Quyền `drive.file`* bên dưới).
- Bản trước dùng một ô keychain chung; sau khi nâng cấp, tài khoản đã đăng nhập ở bản cũ cần đăng nhập lại một lần (mỗi tài khoản một ô riêng).
- Gỡ (đăng xuất) một tài khoản sẽ **tạm dừng** (không xoá) tác vụ của nó; đăng nhập lại đúng tài khoản đó rồi *Thử lại/Tiếp tục* để chạy tiếp.
- **Smart Upload không được khôi phục sau khi tắt app**, và tác vụ tải lên dở dang cũng không lưu: mở lại app thì chọn lại thư mục rồi bấm *Bắt đầu tải lên* (file đã có đúng phiên bản trên Drive được bỏ qua, file còn dở tải lại từ đầu).
- Smart Upload bỏ qua file ẩn (tên bắt đầu bằng `.`), liên kết tượng trưng và file rỗng; quét tối đa 16 cấp thư mục con.
- Thông báo lỗi từ backend Rust hiện chỉ có tiếng Việt (giao diện thì đủ VI/EN/ZH).
- Vị trí *Bộ nhớ dùng chung* (Shared Drive) được backend hỗ trợ nhưng đang bị ẩn bằng CSS trong `index.html`.
- **Quyền `drive.file` (bản mặc định)**: peaDrive chỉ sửa, xoá, đổi tên, chia sẻ được mục do chính nó tạo (các mục khác bị làm mờ trong menu chuột phải) và chỉ tải lên được vào *Drive của tôi* hoặc thư mục do peaDrive tạo. Thư mục trùng tên của bạn được giữ nguyên: peaDrive dùng thư mục `<tên> (peaDrive)` bên cạnh. Bản copy dự phòng khi hết quota tải chỉ chạy nếu Drive cho phép copy file nguồn — chưa kiểm chứng bằng tài khoản thật (xem [`docs/GOOGLE_VERIFICATION.md`](docs/GOOGLE_VERIFICATION.md)). Bản `full-drive` không có các giới hạn này.
- Bản copy dự phòng cần dung lượng trống trong Drive của bạn (tài khoản miễn phí 15 GB). Nếu app bị tắt đột ngột khi đang copy, bản tạm nằm trong thư mục `peaDrive Temp` ở My Drive — có thể xoá tay.
- File Google Docs/Sheets/Slides được xuất sang docx/xlsx/pptx qua `files.export` (API giới hạn 10 MB); giao diện hiện đang làm mờ chúng nên không chọn tải được.
- Chưa ký số (code signing/notarization): Windows SmartScreen / macOS Gatekeeper sẽ cảnh báo với bản tự build.
