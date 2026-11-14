# Kiến trúc peaDrive

```
Tauri 2: một tiến trình, WebView nạp ui/index.html (+ style.css, app.js) qua tauri:// (không có HTTP server cho UI)
┌────────────────┐ fetch('/api/…') ┌───────────┐ invoke('api') ┌───────────┐
│ ui/index.html  │────────────────►│ bridge.js │──────────────►│ src-tauri │
│ + style/app.js │◄────────────────│ (inject)  │◄──────────────│ lib.rs    │
└────────────────┘    Response     └───────────┘     JSON      └─────┬─────┘
                                                                      ▼
                                                      peadrive_core::api::Api::call(method, url, body)
peadrive-core (Rust, tokio)
 api      định tuyến /api/* của index.html → auth / drive / local / manager
 local    duyệt thư mục máy (Máy tính, nơi thường dùng, ổ đĩa, dung lượng trống)
 auth     PKCE ► browser mặc định ► loopback 1 lần 127.0.0.1:port ► token ⇄ Keychain / Credential Manager
 drive    files.get/list/copy/delete ► Range GET ► parser link ► phân loại lỗi quota
 manager  hàng đợi ► N file song song ► job (pause / resume / cancel / jobs.json)
 upload   Smart Upload: smart (worker nền, luật 2 phút) ► uploader (hàng đợi tải lên) ► upload (phiên resumable của Drive)
 manual   Tải lên thủ công: manual (quét phần đã chọn một lần, không luật 2 phút) ► uploader ► upload

 job (1 file)
  ChunkQueue ──► Worker 0 ─Range─► Ring 0 ─┐
  (tăng dần,     Worker 1 ─Range─► Ring 1 ─┼─► Disk writer ─pwrite(offset)─► name.part ─rename─► name
   bỏ chunk      ...                       │   (1 thread, quay vòng các ring)
   đã xong)      Worker N ─Range─► Ring N ─┘
  Governor      429 / rateLimit ► giảm số worker, tăng lại dần
  Manifest      name.peadrive: bitmap chunk đã ghi xuống đĩa ► resume
  Quota         downloadQuotaExceeded ► files.copy vào Drive cá nhân ► tải bản copy ► xoá bản copy
```

## Đường dữ liệu (engine)

| Thành phần | File | Vai trò |
|---|---|---|
| `Plan` / `ChunkMap` / `ChunkQueue` | `engine/plan.rs` | chia file thành chunk; theo dõi chunk đã **ghi xuống đĩa** đủ; phát chunk theo thứ tự tăng dần, bỏ chunk đã xong |
| `Worker` | `engine/worker.rs` | task async: nhận chunk → `GET Range: bytes=a-b` → đẩy từng frame vào ring của mình; lỗi giữa chừng thì thử lại **từ đúng byte đã nhận** |
| `Ring` (SPSC) | `engine/ring.rs` | vòng đệm không khoá, mỗi worker một ring; producer = worker, consumer = writer |
| `Writer` | `engine/writer.rs` | một thread blocking quay vòng các ring, `pwrite` tại offset (Unix `write_all_at`, Windows `seek_write`) |
| `job` | `engine/job.rs` | chuẩn bị (manifest, preallocate) → session → copy dự phòng → verify MD5 → rename |
| `Manager` | `engine/manager.rs` | hàng đợi, giới hạn file song song, snapshot realtime, `jobs.json` |
| `upload` / `uploader` / `smart` / `manual` | `engine/upload.rs`, `uploader.rs`, `smart.rs`, `manual.rs` | tải lên Drive, Smart Upload và Tải lên thủ công: xem các mục *Smart Upload* và *Tải lên thủ công* bên dưới |

### Ring buffer

- Mỗi ring là mảng byte luỹ thừa 2 với hai con trỏ đơn điệu: `head` (consumer) và `tail` (producer đã **công bố**), đồng bộ bằng Acquire/Release — không mutex trên đường dữ liệu.
- Dữ liệu đi theo **record** `[offset u64][len u32][payload]`. Producer copy payload thẳng từ frame mạng vào ring (một memcpy), vá header khi đóng record rồi *commit*. Record đóng khi đủ ~1 MiB, hết chunk, ring đầy hoặc 250 ms.
- Writer chỉ thấy record đã commit đủ ⇒ mỗi record là 1–2 lệnh `pwrite` lớn, ít syscall; dữ liệu quấn vòng được ghi bằng hai đoạn liên tục.
- **Backpressure:** ring đầy ⇒ worker `await` chờ chỗ trống ⇒ ngừng đọc socket ⇒ cửa sổ TCP co lại. RAM luôn bị chặn trên bởi `worker × ring × file song song`. Đĩa chậm không làm nghẽn việc đọc của worker khác.
- Lỗi ghi đĩa ⇒ `Control::fail` ⇒ mọi worker thoát ngay (kể cả đang chờ ring).

### Chống nghẽn I/O

- Preallocate `set_len(size)` trước khi tải; chunk phát theo thứ tự tăng dần nên ghi gần tuần tự (tránh zero-fill xa trên NTFS).
- `Content-Encoding: identity`, HTTP/1.1, mỗi worker một kết nối TCP riêng (nhiều luồng TCP cho thông lượng cao hơn một kết nối HTTP/2).
- Checkpoint manifest mỗi ≥ 3 giây: chụp bitmap → `fsync` dữ liệu → ghi manifest ⇒ bit đã set thì dữ liệu chắc chắn đã bền.

## Quota & lỗi (`drive/quota.rs`)

| Lớp lỗi | Ví dụ | Hành động |
|---|---|---|
| Transient | lỗi mạng, 5xx, luồng cụt, idle > 30 s | backoff `min(30s, 0.5s·2ⁿ)` có jitter, thử lại từ byte đã nhận |
| RateLimited | 429, `userRateLimitExceeded` | như trên **và** `Governor` giảm một nửa số worker; yên ổn 10 s tăng lại 1 |
| DownloadQuota | `downloadQuotaExceeded` | copy server-side vào `peaDrive Temp` (cần đủ dung lượng trống), tải bản copy, xoá bản copy. Với `drive.file`, Drive có thể từ chối copy file nguồn không do app tạo → báo lỗi kèm cách tạo bản sao thủ công (xem [Quyền Drive](#quyền-drive-scope)) |
| Auth | 401 sau refresh | `Auth` refresh đơn luồng; thất bại ⇒ yêu cầu đăng nhập lại |
| Fatal | 404, `cannotDownloadFile`, `dailyLimitExceeded` | dừng, báo lỗi |

Bản copy dự phòng được ghi vào manifest (`copy_id`): **tạm dừng** thì giữ để resume khỏi copy lại; xong/lỗi/huỷ thì xoá (trừ khi bật "giữ bản copy").

## Resume & khôi phục

- `<tên>.part` + `<tên>.peadrive` (JSON: file id, size, md5, modifiedTime, chunk_size, bitmap base64, copy_id). Manifest chỉ dùng lại khi trùng id/size/md5/modifiedTime với metadata hiện tại.
- Chunk đang dở khi tạm dừng sẽ tải lại từ đầu chunk (tối đa một chunk mỗi worker).
- `jobs.json` lưu danh sách job chưa xong; mở lại app thì job ở trạng thái *Tạm dừng* ("Khôi phục từ phiên trước").
- Đích: file cùng tên **và** cùng dung lượng ⇒ *Bỏ qua*; khác dung lượng ⇒ `tên (1).ext`; có manifest khớp ⇒ tiếp tục bản dở.

## Smart Upload (tải lên tự động)

```
 UI (modal Smart Upload) ─POST /api/transfer/smart-upload {folder_id, paths, extensions}─► Manager::smart_start
   └─► engine/smart.rs: mỗi thư mục nguồn một Watch (tokio task), quét mỗi 10 s
         scan_folder ─► file đúng đuôi, > 0 byte, mở đọc được, mtime cách now ≥ 2 phút (SMART_MIN_AGE)
         decide      ─► đối chiếu thư mục Drive đích: Create | Skip | Replace(id)
         enqueue     ─► engine/uploader.rs (hàng đợi tải lên, tối đa `parallel_files` file cùng lúc)
                          └─► engine/upload.rs: kiểm tra lại luật 2 phút ► phiên resumable ► chunk ► xác nhận dung lượng
```

| Thành phần | File | Vai trò |
|---|---|---|
| `DriveClient::start_upload` / `upload_chunk` / `upload_status` | `drive/client.rs` | `POST /upload/drive/v3/files?uploadType=resumable` (hoặc `PATCH …/files/{id}` để ghi đè) → địa chỉ phiên; mỗi chunk là `PUT` kèm `Content-Range`; `308` + `Range` = còn thiếu, `200/201` = xong; `PUT` rỗng `bytes */tổng` hỏi lại đã nhận tới đâu. Client riêng `up`: không theo redirect, timeout theo kích thước chunk |
| `upload::upload` | `engine/upload.rs` | pipeline một file: `stat_source` (luật 2 phút) → đối chiếu lại Drive (`decide`, `UpSpec::reconcile`) → phiên → `send_chunks` (đọc chunk ở offset, kiểm tra file không đổi, gửi, nhớ mốc `committed`) → `finish` (Drive phải báo đúng dung lượng). Lỗi tạm thời: backoff rồi **hỏi lại phiên** để gửi tiếp đúng chỗ; phiên 404/410: mở lại từ đầu; hết dung lượng Drive: lỗi tiếng Việt rõ ràng. Tạm dừng/huỷ ngắt cả lời gọi mạng đang treo (`with_control`, kiểm tra mỗi 200 ms) |
| `Uploader` | `engine/uploader.rs` | hàng đợi tải lên (cùng bộ đếm ID với tải xuống), pause / resume / cancel / retry, dòng `direction = "upload"` trong `transfer/queue`; tác vụ Smart Upload bị lỗi tự xếp hàng lại sau 5 phút |
| `Watch` / `scan_folder` | `engine/smart.rs` | worker nền theo dõi thư mục: quy tắc quét, xếp hàng; `decide` (so khớp với file cùng tên trên Drive) nằm ở `engine/upload.rs` |

**Luật 2 phút được kiểm tra ở hai nơi**: `scan_folder` (lúc phát hiện, dùng `std::fs::metadata` mới thay vì số liệu cache của `DirEntry` — trên Windows có thể cũ với file đang được ghi) và `upload::stat_source` (lúc thật sự bắt đầu gửi). Trong lúc gửi, mỗi chunk đều so lại (dung lượng, mtime) với lúc bắt đầu; lệch → lỗi và để watcher tải lại khi file yên.

**Chống tải trùng, không cần lưu trạng thái:** mỗi file tải lên được gắn `appProperties.pdSrc = "<dung lượng>:<mtime ms>"` và `modifiedTime` = ngày sửa của file nguồn. Quyết định được đưa ra hai lần bằng cùng một hàm `decide`: lúc quét (liệt kê thư mục Drive đích một lần, để không xếp hàng file đã có) và lại ngay lúc tác vụ bắt đầu gửi (`find_named` theo tên; chống chạy đua: file bị ghi lại giữa chừng, tác vụ lỗi được xếp hàng lại, tác vụ trùng nhau — kết quả là dòng *Đã có sẵn*, không sinh bản trùng). Bảng quyết định:

| Trên Drive có file cùng tên… | Quyết định |
|---|---|
| không có | `Create` |
| do peaDrive tải lên, chữ ký khớp | `Skip` |
| do peaDrive tải lên, chữ ký khác (file nguồn đã render lại) | `Replace` — ghi đè nội dung, giữ id/link |
| tải tay (không có chữ ký), cùng dung lượng | `Skip` |
| tải tay, khác dung lượng | `Replace` (với `drive.file`: `Create` — không ghi đè được file không do app tạo) |

Trong một phiên chạy, `seen[path] = (dung lượng, mtime)` ngăn xếp hàng lại cùng một phiên bản. Tác vụ tải lên và watcher **không** được lưu qua lần mở app sau (xem README, *Giới hạn đã biết*).

## Xác thực

1. Tạo `code_verifier` (64 byte ngẫu nhiên, base64url) và `code_challenge = BASE64URL(SHA256(verifier))`, cùng `state` ngẫu nhiên.
2. Mở `accounts.google.com/o/oauth2/v2/auth?...&code_challenge_method=S256&access_type=offline&prompt=consent` bằng trình duyệt mặc định (`webbrowser`).
3. Socket loopback `127.0.0.1:<cổng ngẫu nhiên>` nhận đúng một request có `state` khớp (request lạ bị bỏ qua), trả trang "đã xong", rồi đóng. Hết hạn sau 180 s.
4. Đổi `code` + `code_verifier` ở `oauth2.googleapis.com/token`; lưu token (kèm trường `scope` Google trả về) vào Keychain/Credential Manager. Người dùng có thể bỏ tick từng quyền ở màn hình đồng ý: thiếu quyền xem/tải ⇒ thu hồi token vừa cấp và từ chối đăng nhập.
5. Mỗi request lấy token qua `TokenSource` (cache, tự refresh trước hạn 60 s, một lần refresh cho mọi worker).

## Giao diện: tốc độ và thanh trạng thái

Tốc độ hiển thị ở ba nơi (dòng trạng thái `#sbText`, tiêu đề hàng đợi `#qSummary`, từng hàng đợi) và luôn đi qua `speedHTML()` trong `ui/app.js`: số (`.spd-n`, căn phải, `min-width: 5ch`) + đơn vị (`.spd-u`, căn trái, `min-width: 4ch`), chữ số đều nhau (`font-variant-numeric: tabular-nums`). Nhờ khung cố định đó, tốc độ nhảy từ `3,0 MB/s` lên `24,5 MB/s` không làm chữ xê dịch hay đẩy phần tử bên cạnh. Đừng ghép tốc độ vào chuỗi thường.

Chấm trạng thái `#sbDot` luôn là phần tử cuối (ngoài cùng bên phải) của `.statusbar`; chữ trạng thái và dung lượng Drive co lại bằng "…" khi thanh hẹp thay vì đẩy chấm ra khỏi thanh. Dấu ` · ` trước tốc độ nằm trong `span.spd-sep` riêng để câu trạng thái vẫn là một nút văn bản nguyên vẹn và được dịch (`Đang tải lên ({}%) · còn {} file`).

## Tải lên thủ công

```
UI (nút Tải lên / kéo-thả / chuột phải) ─POST /api/transfer/upload {folder_id, paths, use_fallback?}─► Manager::upload_start
  └─► engine/manual.rs::start
        scan_selection   ─► (spawn_blocking) file đã chọn + mọi thư mục kể cả rỗng; bỏ file ẩn, symlink, file 0 byte (báo trong warnings)
        resolve_dest     ─► gốc, hoặc thư mục ghi được. Không ghi được (drive.file) và chưa có use_fallback ⇒ trả needs_fallback MÀ CHƯA LÀM GÌ;
                            có use_fallback ⇒ "Drive của tôi › peaDrive Uploads › <tên thư mục đang mở>" (thư mục do app tạo)
        remote_dir       ─► dựng cây thư mục Drive (ensure_folder, nhớ ID từng cấp)
        list_children    ─► mỗi thư mục Drive một lần; decide_with ─► Skip / Create / Replace (cùng luật với Smart Upload)
        uploader::enqueue─► cùng hàng đợi + pipeline resumable như Smart Upload (reconcile = true, min_age = None)
```

Khác Smart Upload: chạy một lần, không có `Watch`, không lọc đuôi file, không bắt file "yên" 2 phút. Việc không ghi đè file của người dùng được bảo vệ **hai lớp**: `manual::start` quyết định lúc xếp hàng và `upload::upload` quyết định lại lúc tải thật (với quyền thực của phiên). Quét thư mục dùng chung giới hạn `MAX_DEPTH`/`MAX_ENTRIES` với Smart Upload.

## Quyền Drive (scope)

peaDrive xin scope theo **bản build** (`config::OAUTH_SCOPES`, chọn bằng cờ Cargo `full-drive`; xem [`FLAVORS.md`](FLAVORS.md)): bản mặc định (*public*) xin hai scope `drive.readonly` (restricted) và `drive.file` (non-sensitive); bản `full-drive` (*personal*) xin đúng một scope `drive` (restricted, toàn quyền). Quyền thực tế của từng phiên được suy ra từ trường `scope` Google trả về lúc cấp token, lưu cùng token (`Tokens.scope`) và đọc qua `TokenSource::grants()` → `auth::Grants { read_all, write_own, write_all }`. Mã chạy KHÔNG rẽ nhánh theo bản build: mọi hạn chế bên dưới đều suy ra từ `Grants` lúc chạy, nên hằng `OAUTH_SCOPES` là khác biệt duy nhất giữa hai bản.

| Nhóm | Lời gọi | Bản mặc định (`drive.readonly` + `drive.file`) |
|---|---|---|
| Đọc | `files.list/get`, `alt=media` (Range), `files.export`, `about.get`, `drives.list` | mọi file người dùng thấy được |
| Tạo mới | `files.create` (thư mục mới, tải lên resumable) | vào gốc *Drive của tôi* hoặc thư mục do app tạo |
| Sửa / xoá / chia sẻ | `files.update` (đổi tên, thùng rác, ghi đè), `files.delete`, `permissions.create` | chỉ mục do app tạo |
| Copy dự phòng | `files.copy` | chỉ khi Drive cho copy file nguồn (cần kiểm chứng bằng tài khoản thật) |

Bản `full-drive` (scope `drive`) làm được tất cả các dòng trên với mọi file.

Cách code xử lý ràng buộc này:

- `DriveFile.is_app_authorized` (`isAppAuthorized` trong `FILE_FIELDS`) cho biết app có tạo hoặc được mở mục đó không. `GET /api/drive/list` trả thêm `can_modify` để giao diện làm mờ *Đổi tên / Thùng rác / Xoá / Chia sẻ link* với mục không ghi được.
- Lệnh ghi bị Drive từ chối (403 `appNotAuthorizedToFile`, hoặc 404) khi phiên không có `write_all`: `DriveClient::write_denied` giữ nguyên `status`/`reason` (để `quota::classify` và nhánh "404 → tạo file mới" không đổi) nhưng thay thông báo bằng lời giải thích tiếng Việt.
- `ensure_folder_named`: thư mục trùng tên của người dùng (thấy được nhờ `drive.readonly`, nhưng không ghi được) không bị dùng lại; peaDrive tạo/dùng lại thư mục `"<tên> (peaDrive)"` cạnh nó.
- `smart::start` từ chối ngay thư mục đích không ghi được; `decide_with` không `Replace` file không do app tạo (tạo bản mới); khi nhiều file cùng tên, `pick_existing` ưu tiên bản do peaDrive tải lên. `manual::start` (Tải lên thủ công) thì trả `needs_fallback` để giao diện hỏi người dùng rồi tải vào `peaDrive Uploads › <tên thư mục đang mở>`.
- Phiên có scope `drive` (`write_all = true`: bản `full-drive`, hoặc token cố định trong test) bỏ qua mọi hạn chế trên: menu không bị làm mờ, không tạo thư mục `(peaDrive)`, Tải lên thủ công không cần `needs_fallback`.
- OAuth client (Client ID / secret) **khoá cứng lúc build**: `build.rs` đọc `.env.public` (mặc định) hoặc `.env.personal` (cờ `full-drive`) ở gốc dự án và nhúng vào `OAuthConfig::builtin()` (`option_env!`); không có `oauth.json`, không đọc biến môi trường hay Cài đặt lúc chạy. Scope luôn lấy từ bản build. Xem [`FLAVORS.md`](FLAVORS.md).
- Test mô phỏng `drive.file` ở `crates/peadrive-core/tests/smart_upload.rs` (Drive giả trả `isAppAuthorized` và 403 `appNotAuthorizedToFile`); đăng nhập thiếu quyền ở `tests/e2e.rs`.
- Các assert phụ thuộc bản build (chuỗi scope trong URL đăng nhập, `Grants` mặc định, `can_modify` của shortcut) dùng `cfg!(feature = "full-drive")`, nên `cargo test -p peadrive-core` và `cargo test -p peadrive-core --features full-drive` dùng chung một bộ test.

## Mô hình luồng

- tokio multi-thread: worker, manager, ticker, loopback, HTTP.
- **1 blocking thread** cho writer mỗi file đang tải; `spawn_blocking` cho fsync/manifest/MD5/keychain.
- UI nhận sự kiện từ `broadcast` của Manager; backend không bao giờ chặn luồng UI.

## Giao diện ↔ backend

Giao diện (`ui/index.html` + `style.css` + `app.js`) chỉ biết `fetch('/api/…')`:

1. `src-tauri/src/bridge.js` được Tauri chèn vào trang (`initialization_script`) trước script của trang; nó thay `window.fetch` cho đường dẫn `/api/*` bằng `invoke('api', { method, url, body })` và dựng lại `Response` (lỗi → HTTP 400 kèm chuỗi lỗi để UI hiện toast).
2. Lệnh `api` (một lệnh Tauri duy nhất) chuyển vào `peadrive_core::api::Api::call` — toàn bộ định tuyến nằm trong lõi nên test được không cần cửa sổ.
3. Cửa sổ tắt `dragDropEnabled` (nếu bật, WebView2 chặn kéo-thả HTML5 giữa hai khung) và CSP cho phép inline (trang dùng `onclick` inline).

| Endpoint UI gọi | Xử lý |
|---|---|
| `status`, `login`, `disconnect`, `accounts`, `accounts/switch`, `accounts/remove`, `drive/about` | `login` chỉ **mở** trình duyệt (luôn có hộp chọn tài khoản) rồi trả về ngay; UI hỏi `status` tới khi `signins` tăng. `status` kèm `accounts` / `active` / `blocked`. **`accounts/switch` tức thì** (không mạng, không trình duyệt; tài khoản mất token → `relogin:true`). `accounts/remove` (và `disconnect` = gỡ tài khoản đang dùng) tạm dừng tác vụ của tài khoản đó rồi chuyển sang tài khoản còn lại |
| `drive/link?link=` | phân giải link người dùng dán (file/thư mục, kể cả `resourcekey`) cho **Link Mode** |
| `drive/list?location=&folder_id=&rk=` | `link` (nội dung một link, kèm resourcekey) · `my` · `shared` · `starred` · `trash` · `storage` (file của tôi theo dung lượng giảm dần, tối đa 500) · `team` (Shared Drive); ID khác = thư mục con (trong `trash` không lọc `trashed`) |
| `drive/mkdir`, `rename`, `delete`, `share` | Drive API (xoá thường = vào thùng rác; `permanent` = xoá vĩnh viễn; `share` = "ai có link đều xem") |
| `local/list`, `local/reveal` | `local.rs`; trang `::drives` = nơi thường dùng + ổ đĩa; dung lượng ổ trên Windows đọc thẳng bằng `GetDiskFreeSpaceExW` (không qua `fs4`, vì `fs4` gọi `GetVolumePathNameW` trước và hàm đó lỗi trên ổ RAM disk ImDisk) |
| `transfer/download`, `transfer/link` | `Manager::enqueue` (thư mục → mở rộng đệ quy thành từng file) |
| `transfer/queue` | job (tải xuống **và** tải lên, theo `direction`) → `waiting/working/paused/restored/held/completed/skipped/failed/cancelled`; pha đặc biệt (copy dự phòng, MD5) hiện qua `error_msg`; `smart` = số thư mục Smart Upload đang theo dõi; `ready` / `ready_reason` (`not_signed_in` \| `offline`) = trạng thái cho chấm ở thanh dưới: đã đăng nhập + liên lạc được với Google (`DriveClient::reachable`). `/api/status` cũng có hai trường này |
| `transfer/pause|resume|cancel|retry|clear`, `session` | tạm dừng chung, huỷ tất cả, thử lại, dọn danh sách; phiên khôi phục: `resume` / `discard` / `later` / `auto_on|off` |
| `update/check`, `update/open`, `open` | hỏi máy chủ phiên bản của bạn (`check_version.php`); `force:true` ⇒ app bị chặn (xem dưới); `url` trả về là liên kết tải thẳng `UPDATE_URL` (`download.php?app=peadrive&os=win\|mac`, `config.rs`); mở link chỉ chấp nhận `https` |
| `settings` (do cầu nối dùng) | chỉ còn thiết lập cá nhân (hiệu năng, quota & toàn vẹn); hộp thoại mở bằng nút bánh răng hoặc **Ctrl/⌘ + ,**. Thông số OAuth khoá cứng lúc build từ `.env.<flavor>` (xem `build.rs`), không đọc từ file, biến môi trường hay Cài đặt; khoá `oauth` do giao diện cũ gửi lên (nếu có) bị bỏ qua |
| `transfer/smart-upload` | `POST {folder_id, paths, extensions}` bắt đầu theo dõi thư mục (`Manager::smart_start`); `GET` liệt kê; `POST …/stop?id=` dừng một thư mục (bỏ `id` = dừng tất cả) |
| `transfer/upload` | `POST {folder_id, paths, use_fallback?}` tải lên thủ công (`Manager::upload_start`) → `{queued, skipped, folders, destination, warnings}`; đích không ghi được và chưa có `use_fallback` ⇒ `{needs_fallback, blocked_name, destination}` (chưa làm gì) |

Thử giao diện thật: `cd tools/ui-smoke && npm install && npm test` nạp `index.html` + `bridge.js` trong jsdom, với JSON do chính `Api::call` sinh ra (xuất từ test `ui_api_contract` bằng `PEADRIVE_DUMP=<thư mục>`).

## Đa tài khoản

- `auth::Auth` giữ danh sách tài khoản (`accounts.json`: id = email viết thường, tên) và **một bộ token cho mỗi tài khoản** trong Keychain / Credential Manager (service `peaDrive` / `google-oauth:<email>`; bản `full-drive` dùng service `peaDrive-personal`). Đăng nhập xong hỏi Drive `about` để biết email; trùng email thì cập nhật token.
- `drive::ClientPool` tạo `DriveClient` riêng cho từng tài khoản (token riêng, refresh đơn luồng riêng).
- **Mỗi tác vụ ghi `account` lúc tạo** (lưu cả trong `jobs.json`) và luôn chạy bằng client của tài khoản đó, nên đổi tài khoản trên giao diện không làm tác vụ đang chạy dùng nhầm token. Thao tác duyệt/tải mới dùng tài khoản đang chọn.
- Chuyển tài khoản (`Auth::switch`) chỉ đổi con trỏ `active` + ghi `accounts.json` — không gọi mạng. Gỡ tài khoản: thu hồi refresh token, xoá ô keychain, tạm dừng tác vụ của nó, chuyển sang tài khoản còn lại.

## Link Mode

Giao diện cất trạng thái Drive của tôi, dựng một trạng thái Drive mới có `linkRoot` và `location = "link"`; mọi thứ còn lại (sắp xếp, tìm kiếm, chọn nhiều, kéo-thả, breadcrumb) dùng lại nguyên mã cũ. Backend: `drive/link` phân giải gốc, `drive/list?location=link&folder_id=…&rk=…` liệt kê (gửi `X-Goog-Drive-Resource-Keys: <id>/<rk>`), `transfer/download` nhận `resource_key` của từng mục và chuyển tiếp tới cả metadata lẫn các request `Range`.

## Kiểm tra phiên bản và chặn app

`Api::startup_check` (shell Tauri spawn ngay khi mở app) → `update::check` gọi `check_version.php` (timeout 8 s). Kết quả `force_update` đặt cờ `blocked`: `Api::call` từ chối mọi route (trừ `status`, `update/check`, và mở đúng URL cập nhật) bằng lỗi `E_OUTDATED:`, đồng thời `Manager::pause_all()`. Lớp phủ + modal nằm trong `bridge.js` (không phụ thuộc `index.html`): vô hiệu (`inert`) mọi phần tử bên dưới, chặn Esc/Tab/phím tắt/dán/thả, tự dựng lại nếu bị gỡ khỏi DOM. Máy chủ lỗi/không với tới → **không chặn**.
