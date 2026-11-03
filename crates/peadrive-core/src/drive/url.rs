use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveRef {
    pub id: String,
    /// Link chia sẻ cũ (từ 09/2021) kèm `resourcekey`; phải gửi qua header `X-Goog-Drive-Resource-Keys`.
    pub resource_key: Option<String>,
}

fn valid_id(s: &str) -> bool {
    s.len() >= 10 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Nhận link file/thư mục Drive (file/d/…, open?id=, uc?id=, drive/folders/…, docs.google.com/…/d/…) hoặc ID trần.
pub fn parse_drive_input(input: &str) -> Result<DriveRef> {
    let s = input.trim().trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>');
    if s.is_empty() {
        return Err(Error::BadInput("link trống".into()));
    }
    if let Ok(u) = url::Url::parse(s) {
        let host = u.host_str().unwrap_or("");
        if host != "google.com" && !host.ends_with(".google.com") {
            return Err(Error::BadInput(format!("không phải link Google Drive: {s}")));
        }
        let mut id = None;
        let mut resource_key = None;
        for (k, v) in u.query_pairs() {
            match k.as_ref() {
                "id" => id = Some(v.into_owned()),
                "resourcekey" => resource_key = Some(v.into_owned()),
                _ => {}
            }
        }
        if id.is_none() {
            let segs: Vec<&str> = u.path_segments().map(|p| p.collect()).unwrap_or_default();
            id = segs.windows(2).find(|w| (w[0] == "d" || w[0] == "folders") && valid_id(w[1])).map(|w| w[1].to_string());
        }
        return match id.filter(|i| valid_id(i)) {
            Some(id) => Ok(DriveRef { id, resource_key }),
            None => Err(Error::BadInput(format!("không tìm thấy ID Drive trong link: {s}"))),
        };
    }
    if valid_id(s) {
        return Ok(DriveRef { id: s.to_string(), resource_key: None });
    }
    Err(Error::BadInput(format!("không nhận dạng được link/ID: {s}")))
}

/// Tách nhiều link từ văn bản dán vào (xuống dòng, khoảng trắng, dấu phẩy).
pub fn split_inputs(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1A2b3C4d5E6f7G8h9J0kLmNoPqRsTuVwX";

    fn id_of(s: &str) -> String {
        parse_drive_input(s).unwrap().id
    }

    #[test]
    fn many_shapes() {
        assert_eq!(id_of(&format!("https://drive.google.com/file/d/{ID}/view?usp=sharing")), ID);
        assert_eq!(id_of(&format!("https://drive.google.com/open?id={ID}")), ID);
        assert_eq!(id_of(&format!("https://drive.google.com/uc?export=download&id={ID}")), ID);
        assert_eq!(id_of(&format!("https://drive.google.com/drive/folders/{ID}?usp=sharing")), ID);
        assert_eq!(id_of(&format!("https://drive.google.com/drive/u/0/folders/{ID}")), ID);
        assert_eq!(id_of(&format!("https://docs.google.com/document/d/{ID}/edit")), ID);
        assert_eq!(id_of(ID), ID);
        assert_eq!(id_of(&format!("  \"{ID}\"  ")), ID);
    }

    #[test]
    fn resource_key() {
        let r = parse_drive_input(&format!("https://drive.google.com/file/d/{ID}/view?resourcekey=0-AbC_d")).unwrap();
        assert_eq!(r.resource_key.as_deref(), Some("0-AbC_d"));
    }

    #[test]
    fn rejects_bad() {
        assert!(parse_drive_input("").is_err());
        assert!(parse_drive_input("https://example.com/file/d/1A2b3C4d5E6f7G8h9J0k").is_err());
        assert!(parse_drive_input("https://drive.google.com/").is_err());
        assert!(parse_drive_input("abc").is_err());
    }

    #[test]
    fn splits() {
        assert_eq!(split_inputs("a b\nc,d\r\n\n e").len(), 5);
    }
}
