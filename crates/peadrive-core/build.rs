//! Khoá cứng OAuth client vào bản build.
//!
//! Đọc file theo flavor ở GỐC workspace — `.env.public` (mặc định) hoặc `.env.personal` (cờ Cargo `full-drive`) —
//! rồi đặt `PEADRIVE_EMBEDDED_CLIENT_ID` / `PEADRIVE_EMBEDDED_CLIENT_SECRET` để `OAuthConfig::builtin()` (`config.rs`)
//! nhúng bằng `option_env!`. Flavor chọn file bằng đúng cờ quyết định bộ scope, nên không thể nhúng nhầm client của
//! flavor kia.
//!
//! Cố ý KHÔNG đọc `PEADRIVE_CLIENT_*` từ biến môi trường của shell: file là nguồn duy nhất, nên biến còn sót lại từ
//! flavor khác không lọt được vào bản build này. App không đọc client từ file, biến môi trường hay Cài đặt lúc chạy.
//! Xem docs/FLAVORS.md.

#[path = "build_env.rs"]
mod build_env;

use std::path::PathBuf;
use std::{env, fs};

const ID_KEY: &str = "PEADRIVE_CLIENT_ID";
const SECRET_KEY: &str = "PEADRIVE_CLIENT_SECRET";

fn main() {
    let personal = env::var_os("CARGO_FEATURE_FULL_DRIVE").is_some();
    let name = if personal { ".env.personal" } else { ".env.public" };
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo luôn đặt CARGO_MANIFEST_DIR"));
    let path = manifest.join("..").join("..").join(name);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_env.rs");
    // File chưa tồn tại thì cargo coi là "đổi" và chạy lại script ở mỗi lần build — chủ ý: tạo file xong là có hiệu lực ngay.
    println!("cargo:rerun-if-changed={}", path.display());

    let (id, secret) = match fs::read_to_string(&path) {
        Ok(text) => {
            let pairs = build_env::parse(&text);
            (
                build_env::get(&pairs, ID_KEY).unwrap_or("").to_string(),
                build_env::get(&pairs, SECRET_KEY).unwrap_or("").to_string(),
            )
        }
        Err(_) => (String::new(), String::new()),
    };

    if id.is_empty() || secret.is_empty() {
        let msg = format!(
            "{name} thiếu hoặc chưa điền {ID_KEY} / {SECRET_KEY}: bản build này sẽ KHÔNG đăng nhập được Google. \
             Sao chép {name}.example thành {name} ở gốc dự án rồi điền (xem docs/FLAVORS.md)."
        );
        // Bản phát hành mà thiếu client thì dừng hẳn, tránh đóng gói một bộ cài không đăng nhập được.
        if env::var("PROFILE").as_deref() == Ok("release") {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
        println!("cargo:warning={msg}");
        return;
    }

    println!("cargo:rustc-env=PEADRIVE_EMBEDDED_CLIENT_ID={id}");
    println!("cargo:rustc-env=PEADRIVE_EMBEDDED_CLIENT_SECRET={secret}");
}
