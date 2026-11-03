# Xác minh OAuth với Google

> Tóm tắt checklist theo trang [Verification requirements](https://support.google.com/cloud/answer/13464321) của Google (tra cứu 10/2026). Đây **không** phải tài liệu chính sách: luôn đối chiếu bản mới nhất trước khi nộp.

> **Phạm vi:** tài liệu này chỉ áp dụng cho bản **public** (mặc định, scope `drive.readonly` + `drive.file`). Bản `full-drive` (cá nhân, scope `drive`) dùng một project / OAuth client **riêng** (secret `PEADRIVE_PERSONAL_*`), giữ ở chế độ Testing và **không** nộp xác minh — xem [`FLAVORS.md`](FLAVORS.md).

## 1. peaDrive xin scope nào, thuộc nhóm nào

| Scope | Nhóm theo Google | peaDrive dùng để |
|---|---|---|
| `https://www.googleapis.com/auth/drive.readonly` | **Restricted** | duyệt, liệt kê, tải xuống, export file; đọc dung lượng |
| `https://www.googleapis.com/auth/drive.file` | Non-sensitive | tạo file/thư mục mới (Tải lên thủ công, Smart Upload, thư mục mới); sửa/xoá/chia sẻ chỉ mục do chính app tạo |

Vì có `drive.readonly`, đường đi là **restricted scope verification** (brand verification + scope verification + đánh giá bảo mật, xem mục 5). Thu hẹp từ `drive` xuống hai scope này là điều kiện cần — Google chỉ chấp nhận scope hẹp nhất — nhưng không đổi nhóm của app.

## 2. Cấu hình Cloud Console (phải khớp mã nguồn)

- *OAuth consent screen → Data access*: đúng hai scope ở trên, không thừa không thiếu (= `config::OAUTH_SCOPES`). Test `oauth_pkce_flow_against_mock_token_endpoint` khoá việc URL đăng nhập chỉ xin hai scope này.
- Loại client: **Desktop app**. Publishing status: **In production** sau khi nộp (ở *Testing*, refresh token chỉ sống 7 ngày).
- *Project contact information* luôn cập nhật: Google liên hệ qua đó.

## 3. Brand verification (mọi app)

- **Homepage** trên domain bạn sở hữu; mô tả chức năng app (không chỉ là trang đăng nhập); có link tới Privacy Policy, trùng với link khai trên consent screen.
- **Privacy Policy** cùng domain với homepage; nêu rõ app truy cập, dùng, lưu, chia sẻ dữ liệu Google thế nào; tuân thủ *Limited Use*.
- **Xác minh domain** trong Authorized domains bằng Google Search Console (tài khoản owner/editor của project).
- **Google branding**: nút khởi tạo đăng nhập Google phải theo [hướng dẫn thương hiệu](https://developers.google.com/identity/branding-guidelines).
- **Thông báo quyền riêng tư trong app** (in-product privacy notification) hiển thị rõ trong giao diện.

## 4. Thêm cho restricted scope

1. **Use case phù hợp.** Google chỉ cho một số loại app dùng restricted scope của Drive; chức năng app sẽ được xem xét.
2. **Video demo**: toàn bộ luồng cấp quyền OAuth; đúng tên/thương hiệu app đã nộp; *toàn bộ* consent screen hiển thị đúng các scope xin (chuyển ngôn ngữ sang **English** ở góc trái dưới); cho thấy từng chức năng dùng từng scope.
3. **Limited Use**: dữ liệu từ Google API chỉ dùng cho tính năng hiển thị rõ trong app; không chuyển giao, bán, dùng quảng cáo.
4. **Scope hẹp nhất + biện minh chi tiết** (mục 6), nêu rõ vì sao scope hẹp hơn không đủ và chức năng nào sẽ hỏng.
5. **Đánh giá bảo mật** (xem mục 5).

## 5. Đánh giá bảo mật (CASA) — chưa có xác nhận miễn

- Trang *Verification requirements* nói app dùng restricted scope phải nộp đánh giá bảo mật **hằng năm** từ đơn vị được Google chỉ định.
- Trang scope của Drive API gắn nghĩa vụ này với việc **lưu hoặc truyền dữ liệu restricted lên máy chủ**. peaDrive chạy cục bộ: token nằm trong Keychain / Credential Manager, dữ liệu Drive đi thẳng từ Google về máy, lời gọi duy nhất tới máy chủ của bạn là kiểm tra phiên bản (chỉ gửi tên app và số phiên bản — `update.rs`).
- Trong các trang tôi đã đọc (Verification requirements, Security Assessment, When is verification not needed, FAQ) **không có ngoại lệ riêng cho app local-only**. Hãy nêu rõ kiến trúc local-only trong hồ sơ, hỏi thẳng đội xác minh, và chuẩn bị phương án nếu Google vẫn yêu cầu đánh giá.

## 6. Văn bản biện minh scope (tiếng Anh, dán vào form)

**`drive.readonly`**

> peaDrive is a desktop application (Windows/macOS) that lets a user browse their own Google Drive and files shared with them, and download the files and folders they choose to their computer at high speed using parallel ranged requests. It calls files.list and files.get to list items and read metadata, files.get with alt=media and files.export to download content, about.get to show storage usage, and drives.list to show shared drives. A narrower scope would not work: drive.file only exposes files that the app itself created or that the user explicitly opened with the app, so the user's existing files and folders, which are exactly what they want to download, could not be listed or downloaded at all; drive.metadata.readonly does not allow downloading file content. All data stays on the user's computer. peaDrive has no backend that receives Google user data; Google tokens are stored in the operating system credential store.

**`drive.file`**

> peaDrive uploads files and folders from the user's computer to the user's own Drive with resumable uploads. The user either selects files or folders and presses Upload (or drags them onto the Drive pane), or turns on Smart Upload, which watches a local folder chosen by the user (for example the output folder of a video rendering program) and uploads newly finished files. The app creates new folders and files, and updates only files it uploaded earlier (files.create, files.update). When the destination folder was not created by the app, the app asks the user and uploads into a folder it creates itself ("peaDrive Uploads") instead of writing into the user's own folders. The user can also create a folder from the app. drive.file is the narrowest scope that allows creating files. The app never requests broader write access and does not modify, delete or share files it did not create.

## 7. Việc còn lại phía bạn

- [x] Thông báo quyền riêng tư trong giao diện — **đã có**: màn hình đăng nhập (ngay dưới nút *Kết nối Google Drive*) và hộp thoại *Giới thiệu* (bấm vào số phiên bản ở chân thanh bên), đều trỏ tới <https://peamark.com/privacy> (hằng `PRIVACY_URL` trong `ui/app.js`; có bản dịch VI/EN/ZH). Nội dung có đủ câu cam kết *Limited Use* theo cách Google yêu cầu.
- [ ] **Đối chiếu trang chính sách** <https://peamark.com/privacy> với thông báo trong app. Khi kiểm tra, công cụ của tôi chỉ thấy tiêu đề trang (không thấy nội dung), có thể do trang dựng bằng JavaScript nên tôi chưa đọc được nó. Bạn hãy tự kiểm tra: (a) nội dung hiện ngay khi tải trang (thử "Xem nguồn trang" hoặc tắt JavaScript); (b) mô tả dữ liệu Google nào được truy cập (xem/tải file Drive, tạo file do app tải lên) và dùng để làm gì; (c) nói rõ dữ liệu chỉ xử lý trên máy người dùng, không gửi/bán/chia sẻ cho bên thứ ba; (d) cách lưu token (Keychain / Credential Manager) và cách thu hồi quyền; (e) có nguyên văn cam kết *Limited Use*; (f) cùng domain với homepage và link giống hệt link khai trên consent screen.
- [ ] Kiểm tra nút đăng nhập Google theo branding guideline.
- [ ] Thử bằng **tài khoản Google thật** (tài khoản chưa từng cấp quyền cho app), vì các điểm sau chưa kiểm chứng được bằng test giả lập: (a) `files.copy` bản copy dự phòng khi file nguồn của người khác hết quota tải — nếu Drive từ chối thì đừng đưa tính năng này vào video/hồ sơ; (b) tải lên vào *Drive của tôi* và vào thư mục do app tạo; (c) bỏ tick `drive.readonly` ở màn hình đồng ý phải ra thông báo lỗi rõ ràng; (d) mục không phải do app tạo bị làm mờ trong menu chuột phải.
- [ ] Đừng đổi scope sau khi được duyệt: thay đổi scope buộc phải xác minh lại.
