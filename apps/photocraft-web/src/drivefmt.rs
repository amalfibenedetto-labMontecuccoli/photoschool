#![allow(dead_code)]
//! The text and bytes of the Google Drive requests `drive.rs` sends, kept free of the browser so
//! they can be unit tested: query strings, the multipart upload body and reading ids out of answers.

use serde_json::Value;

/// MIME type Drive gives folders.
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Boundary of the multipart upload body; a drawing would have to contain it by chance to break it.
const BOUNDARY: &str = "photoschool_drive_boundary_7f3a91c2";

/// `s` as the inside of a single-quoted Drive query string.
pub fn quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Query for the folder called `name`.
pub fn folder_query(name: &str) -> String {
    format!("name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false", quote(name))
}

/// Query for the file called `name` inside the folder `parent`.
pub fn file_query(name: &str, parent: &str) -> String {
    format!("name = '{}' and '{}' in parents and trashed = false", quote(name), quote(parent))
}

/// The `Content-Type` header and the body of a Drive multipart upload: the JSON `meta` part, then
/// the file `data` of type `mime`.
pub fn multipart(meta: &Value, mime: &str, data: &[u8]) -> (String, Vec<u8>) {
    let mut body = Vec::with_capacity(data.len().saturating_add(512));
    body.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{BOUNDARY}\r\nContent-Type: {mime}\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--").as_bytes());
    (format!("multipart/related; boundary={BOUNDARY}"), body)
}

/// The id of the first file in a `files.list` answer.
pub fn first_id(answer: &Value) -> Option<String> {
    answer.get("files")?.get(0)?.get("id")?.as_str().map(str::to_string)
}

/// The id of a file Drive just created.
pub fn created_id(answer: &Value) -> Option<String> {
    answer.get("id")?.as_str().map(str::to_string)
}

/// The name of the Drive file that keeps the autosaved copy of the drawing called `title`:
/// "Autosalvataggio - <title>.dxf". It is never the drawing's own name, so an autosave can't
/// overwrite what the student saved themselves.
pub fn autosave_name(title: &str) -> String {
    let trimmed = title.trim();
    let lower = trimmed.to_lowercase();
    let stem = if lower.ends_with(".dxf") || lower.ends_with(".dwg") { trimmed.get(..trimmed.len().saturating_sub(4)).unwrap_or(trimmed) } else { trimmed };
    let clean: String = stem.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).take(80).collect();
    let clean = clean.trim();
    format!("Autosalvataggio - {}.dxf", if clean.is_empty() { "Disegno" } else { clean })
}

/// Query for the files inside the folder `parent`.
pub fn folder_files_query(parent: &str) -> String {
    format!("'{}' in parents and trashed = false", quote(parent))
}

/// The drawings in a `files.list` answer: (id, name, modified time as Drive gives it), in the
/// order Drive sent them. Other files and malformed entries are left out.
pub fn drive_files(answer: &Value) -> Vec<(String, String, String)> {
    let Some(list) = answer.get("files").and_then(Value::as_array) else { return Vec::new() };
    list.iter()
        .filter_map(|file| {
            let id = file.get("id")?.as_str()?;
            let name = file.get("name")?.as_str()?;
            if !is_drawing(name) {
                return None;
            }
            let modified = file.get("modifiedTime").and_then(Value::as_str).unwrap_or_default();
            Some((id.to_string(), name.to_string(), modified.to_string()))
        })
        .collect()
}

/// The link that opens a file in the browser, in an answer asked for `webViewLink`.
pub fn web_link(answer: &Value) -> Option<String> {
    answer.get("webViewLink")?.as_str().filter(|l| l.starts_with("https://")).map(str::to_string)
}

/// Whether `name` is a drawing file (kept in Drive); images and PDFs are downloaded instead.
pub fn is_drawing(name: &str) -> bool {
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    matches!(extension.as_deref(), Some("pcraft" | "psd" | "psb" | "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" | "gif" | "bmp" | "tga" | "ora" | "xcf" | "kra" | "svg"))
}

/// The text of the email Drive sends the teacher with the shared drawing.
pub fn share_message(student: &str, file: &str) -> String {
    if student.trim().is_empty() {
        format!("Disegno inviato da un alunno: {file}")
    } else {
        format!("Disegno inviato da {student}: {file}")
    }
}

/// The signed-in user's display name in a `about` answer.
pub fn user_name(answer: &Value) -> Option<String> {
    answer.get("user")?.get("displayName")?.as_str().map(str::to_string)
}

/// At most `max` characters of `text`, for an error message.
pub fn short(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Guards the query escaping: a quote or backslash in a drawing name must not end the string
    /// early and change what the query asks for.
    #[test]
    fn queries_escape_quotes_and_backslashes() {
        assert_eq!(quote("a'b\\c"), "a\\'b\\\\c");
        assert_eq!(file_query("it's.dxf", "id1"), "name = 'it\\'s.dxf' and 'id1' in parents and trashed = false");
        assert!(folder_query("Disegni").contains("application/vnd.google-apps.folder"));
    }

    /// Guards the upload body: the metadata part comes first, the bytes follow untouched, and
    /// the closing boundary ends the body.
    #[test]
    fn multipart_wraps_the_bytes_between_metadata_and_the_closing_boundary() {
        let (content_type, body) = multipart(&json!({"name": "a.dxf"}), "application/dxf", &[1, 2, 3, 255]);
        assert!(content_type.starts_with("multipart/related; boundary="));
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{BOUNDARY}\r\nContent-Type: application/json")));
        assert!(text.contains("{\"name\":\"a.dxf\"}"));
        assert!(body.ends_with(format!("\r\n--{BOUNDARY}--").as_bytes()));
        let payload_at = body.windows(4).position(|w| w == [1, 2, 3, 255]);
        assert!(payload_at.is_some());
    }

    /// Guards reading Drive's answers: a missing or malformed answer is `None`, never a panic.
    #[test]
    fn ids_are_read_from_answers_and_bad_answers_give_none() {
        assert_eq!(first_id(&json!({"files": [{"id": "abc"}]})).as_deref(), Some("abc"));
        assert_eq!(first_id(&json!({"files": []})), None);
        assert_eq!(first_id(&json!({})), None);
        assert_eq!(first_id(&json!({"files": [{"id": 7}]})), None);
        assert_eq!(created_id(&json!({"id": "xyz"})).as_deref(), Some("xyz"));
        assert_eq!(created_id(&json!([])), None);
        assert_eq!(short("héllo wörld", 5), "héllo");
    }

    /// Guards the email text: it names the student when Google says who they are.
    #[test]
    fn share_message_names_the_student_when_known() {
        assert_eq!(share_message("Mario Rossi", "stella.dxf"), "Disegno inviato da Mario Rossi: stella.dxf");
        assert_eq!(share_message("  ", "stella.dxf"), "Disegno inviato da un alunno: stella.dxf");
        assert_eq!(user_name(&json!({"user": {"displayName": "Mario Rossi"}})).as_deref(), Some("Mario Rossi"));
        assert_eq!(user_name(&json!({})), None);
    }

    /// Guards what goes to Drive: only drawings (any letter case); images, PDFs and names
    /// without an extension are downloaded so they can be dragged into the assignment.
    #[test]
    fn only_drawings_go_to_drive_and_links_must_be_https() {
        assert!(is_drawing("stella.dxf"));
        assert!(is_drawing("STELLA.DWG"));
        assert!(!is_drawing("stella.png"));
        assert!(!is_drawing("stella.pdf"));
        assert!(!is_drawing("stella"));
        assert!(!is_drawing("dxf"));
        assert_eq!(web_link(&json!({"webViewLink": "https://drive.google.com/file/d/x/view"})).as_deref(), Some("https://drive.google.com/file/d/x/view"));
        assert_eq!(web_link(&json!({"webViewLink": "javascript:alert(1)"})), None);
        assert_eq!(web_link(&json!({})), None);
    }

    /// Guards the Open from Drive list: only drawings, in Drive's order, and a malformed answer
    /// gives an empty list rather than a panic.
    #[test]
    fn the_drive_list_keeps_only_drawings() {
        let answer = json!({"files": [
            {"id": "1", "name": "stella.dxf", "modifiedTime": "2026-10-25T10:00:00.000Z"},
            {"id": "2", "name": "foto.png", "modifiedTime": "2026-10-25T09:00:00.000Z"},
            {"id": "3", "name": "Autosalvataggio - stella.dxf"},
            {"name": "senza-id.dxf"},
            {"id": 5, "name": "x.dxf"}
        ]});
        let files = drive_files(&answer);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].1, "stella.dxf");
        assert_eq!(files[1], ("3".to_string(), "Autosalvataggio - stella.dxf".to_string(), String::new()));
        assert!(drive_files(&json!({})).is_empty());
        assert!(drive_files(&json!({"files": "no"})).is_empty());
        assert_eq!(folder_files_query("a'b"), "'a\\'b' in parents and trashed = false");
    }

    /// Guards the autosave name: it never equals the drawing's own file name (so it can't overwrite
    /// the student's saved work), drops the extension, and is safe as a Drive file name.
    #[test]
    fn autosave_names_never_collide_with_the_drawing_file() {
        assert_eq!(autosave_name("Drawing2"), "Autosalvataggio - Drawing2.dxf");
        assert_eq!(autosave_name("stella.dxf"), "Autosalvataggio - stella.dxf");
        assert_eq!(autosave_name("STELLA.DWG"), "Autosalvataggio - STELLA.dxf");
        assert_eq!(autosave_name("  "), "Autosalvataggio - Disegno.dxf");
        assert_eq!(autosave_name("a/b:c"), "Autosalvataggio - a_b_c.dxf");
        assert_ne!(autosave_name("stella.dxf"), "stella.dxf");
        assert!(autosave_name(&"x".repeat(500)).chars().count() < 120);
    }
}
