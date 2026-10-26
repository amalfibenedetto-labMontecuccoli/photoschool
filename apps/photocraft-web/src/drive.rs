//! Saving to the student's own Google Drive. Everything Photoschool saves or exports on the web goes
//! through [`deliver`]: it signs the user in with Google (the school account, through Google
//! Identity Services), puts the file in a "Photoschool" folder in their Drive with the `drive.file`
//! scope (the app sees only files it created), makes a link anyone at the school can open and says
//! where it went. When that fails (no network, file too big) the file is downloaded through the
//! browser instead, so work is never lost. `?nodrive` skips Drive and sign-in altogether.

use std::cell::RefCell;

use serde_json::{Value, json};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::js_sys;

use crate::drivefmt;

/// OAuth client of the school's Google Cloud project. It is public by design: a browser app has
/// no secret, and Google only accepts sign-ins from the origins registered for it.
const CLIENT_ID: &str = "941646642229-v1amlshet3qalpu0qunksu7bee83dqc9.apps.googleusercontent.com";
/// Only the files this app creates; never the rest of the student's Drive.
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
/// Pre-selects the school's accounts in Google's chooser.
const DOMAIN_HINT: &str = "smpavullo.com";
const GIS_URL: &str = "https://accounts.google.com/gsi/client";
const FOLDER_NAME: &str = "Photoschool";
const DRIVE_URL: &str = "https://www.googleapis.com/drive/v3";
const FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";
/// Larger files are downloaded instead of sent to Drive.
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
/// A token this close to its end is replaced before use.
const TOKEN_MARGIN_MS: f64 = 60_000.0;
const ERROR_TEXT_CHARS: usize = 200;

struct Token {
    value: String,
    expires_at_ms: f64,
}

/// A saved file: the token used and what Drive says about it.
struct Saved {
    token: String,
    id: String,
    link: Option<String>,
    /// The link to the "Photoschool" folder in Drive.
    folder: String,
}

thread_local! {
    static TOKEN: RefCell<Option<Token>> = const { RefCell::new(None) };
    /// A link ready to show: the file name, its link and the folder's link, taken by the app's
    /// next frame.
    static LINK: RefCell<Option<(String, String, String, String)>> = const { RefCell::new(None) };
    /// The signed-in student's name.
    static USER: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The student's files found in Drive (id, name, last change) or why not.
    static LISTING: RefCell<Option<Result<Vec<(String, String, String)>, String>>> = const { RefCell::new(None) };
    /// A file downloaded from Drive (name, contents), taken by the app's next frame.
    static OPENED: RefCell<Option<(String, Vec<u8>)>> = const { RefCell::new(None) };
    /// A message for the student, taken by the app's next frame.
    static NOTICE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Whether the sign-in kept in this browser tab was already looked for.
    static RESTORED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Wakes the app when the background work has something to show.
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

/// Called every frame with the app's egui context, so finished background work can wake it.
pub fn remember_context(ctx: &egui::Context) {
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(ctx.clone());
        }
    });
}

/// A link to a drawing that was just saved, if there is one: take it and show it.
pub fn take_link() -> Option<(String, String, String, String)> {
    LINK.with(|l| l.borrow_mut().take())
}

fn publish_link(name: &str, link: &str, folder: &str, id: &str) {
    LINK.with(|l| *l.borrow_mut() = Some((name.to_string(), link.to_string(), folder.to_string(), id.to_string())));
    wake();
}

/// Ask the app for another frame, so it shows what background work just changed.
fn wake() {
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.request_repaint();
        }
    });
}

const SESSION_KEY: &str = "photoschool_drive_session";

fn session_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok()?
}

/// Keep the sign-in for the rest of this browser tab (a reload doesn't ask again).
fn store_session() {
    let token = TOKEN.with(|t| t.borrow().as_ref().map(|t| (t.value.clone(), t.expires_at_ms)));
    let (Some((value, expires)), Some(storage)) = (token, session_storage()) else { return };
    let name = USER.with(|u| u.borrow().clone());
    let _ = storage.set_item(SESSION_KEY, &json!({ "token": value, "expires": expires, "name": name }).to_string());
}

/// Take back the sign-in kept by [`store_session`], once per page load, if it is still valid.
fn restore_session() {
    if RESTORED.with(|r| r.replace(true)) {
        return;
    }
    let Some(storage) = session_storage() else { return };
    let Some(kept) = storage.get_item(SESSION_KEY).ok().flatten().and_then(|text| serde_json::from_str::<Value>(&text).ok()) else { return };
    let (Some(value), Some(expires)) = (kept.get("token").and_then(Value::as_str), kept.get("expires").and_then(Value::as_f64)) else { return };
    if expires - TOKEN_MARGIN_MS <= js_sys::Date::now() {
        let _ = storage.remove_item(SESSION_KEY);
        return;
    }
    TOKEN.with(|t| *t.borrow_mut() = Some(Token { value: value.to_string(), expires_at_ms: expires }));
    USER.with(|u| *u.borrow_mut() = kept.get("name").and_then(Value::as_str).map(str::to_string));
}

/// The signed-in student's name while their access is valid, `None` otherwise.
pub fn account() -> Option<String> {
    restore_session();
    let valid = TOKEN.with(|t| t.borrow().as_ref().is_some_and(|t| t.expires_at_ms - TOKEN_MARGIN_MS > js_sys::Date::now()));
    if !valid {
        return None;
    }
    Some(USER.with(|u| u.borrow().clone()).unwrap_or_else(|| "Google".to_string()))
}

/// The access token while it is still good, without ever asking Google (no popup).
fn valid_token() -> Option<String> {
    restore_session();
    let now = js_sys::Date::now();
    TOKEN.with(|t| t.borrow().as_ref().filter(|t| t.expires_at_ms - TOKEN_MARGIN_MS > now).map(|t| t.value.clone()))
}

/// The list of drawings found in Drive, if it is ready: take it and show it.
pub fn take_listing() -> Option<Result<Vec<(String, String, String)>, String>> {
    LISTING.with(|l| l.borrow_mut().take())
}

/// A drawing downloaded from Drive, if one is ready: take it and open it.
pub fn take_opened() -> Option<(String, Vec<u8>)> {
    OPENED.with(|o| o.borrow_mut().take())
}

/// Most bytes of a drawing opened from Drive.
const MAX_OPEN_BYTES: usize = 64 * 1024 * 1024;

/// Open from Drive: look for the student's drawings in "Photoschool".
pub fn list_drive() {
    wasm_bindgen_futures::spawn_local(async {
        let result = list_files().await;
        LISTING.with(|l| *l.borrow_mut() = Some(result));
        wake();
    });
}

async fn list_files() -> Result<Vec<(String, String, String)>, String> {
    let token = access_token().await?;
    let folder = find_or_create_folder(&token).await?;
    let url = format!(
        "{FILES_URL}?q={}&orderBy=modifiedTime%20desc&pageSize=100&fields=files(id,name,modifiedTime)&spaces=drive",
        encode(&drivefmt::folder_files_query(&folder))
    );
    let answer = request(&token, "GET", &url, None, None).await?;
    Ok(drivefmt::drive_files(&answer).into_iter().map(|(id, name, modified)| (id, name, local_time(&modified))).collect())
}

/// Drive's time (RFC 3339, UTC) as the student's clock shows it: "25/10 10:32".
fn local_time(rfc: &str) -> String {
    let date = js_sys::Date::new(&JsValue::from_str(rfc));
    if date.get_time().is_nan() {
        return String::new();
    }
    format!("{:02}/{:02} {:02}:{:02}", date.get_date(), date.get_month() + 1, date.get_hours(), date.get_minutes())
}

/// Download the drawing `id` from Drive for the app to open as `name`.
pub fn open_drive_file(id: &str, name: &str) {
    let (id, name) = (id.to_string(), name.to_string());
    wasm_bindgen_futures::spawn_local(async move {
        match download(&id).await {
            Ok(bytes) => {
                OPENED.with(|o| *o.borrow_mut() = Some((name, bytes)));
                wake();
            }
            Err(e) => notify(&format!("Non riesco ad aprire il file dal Drive: {e}")),
        }
    });
}

async fn download(id: &str) -> Result<Vec<u8>, String> {
    let token = access_token().await?;
    let headers = js_sys::Object::new();
    set(&headers, "Authorization", &JsValue::from_str(&format!("Bearer {token}")))?;
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_headers(&headers);
    let window = web_sys::window().ok_or("nessuna finestra del browser")?;
    let response = JsFuture::from(window.fetch_with_str_and_init(&format!("{FILES_URL}/{}?alt=media", encode(id)), &init))
        .await
        .map_err(js_error)?
        .dyn_into::<web_sys::Response>()
        .map_err(|_| "risposta non valida da Google".to_string())?;
    if !response.ok() {
        return Err(format!("Drive ha risposto {}", response.status()));
    }
    let buffer = JsFuture::from(response.array_buffer().map_err(js_error)?).await.map_err(js_error)?;
    let bytes = js_sys::Uint8Array::new(&buffer);
    if bytes.length() as usize > MAX_OPEN_BYTES {
        return Err("il file è troppo grande".to_string());
    }
    Ok(bytes.to_vec())
}

/// A message for the student, if there is one: take it and show it.
pub fn take_notice() -> Option<String> {
    NOTICE.with(|n| n.borrow_mut().take())
}

/// Whether the student still has to sign in before working (`?nodrive` skips it).
pub fn needs_sign_in() -> bool {
    if drive_disabled() {
        return false;
    }
    restore_session();
    USER.with(|u| u.borrow().is_none())
}

/// The Sign in button: Google's sign-in popup, then the student's name.
pub fn sign_in() {
    wasm_bindgen_futures::spawn_local(async {
        match access_token().await {
            Ok(_) => wake(),
            Err(e) => notify(&format!("Accesso non riuscito: {e}")),
        }
    });
}

/// Forget the account in this tab and the settings kept in this browser, then start again from
/// the sign-in screen, ready for the next student.
pub fn sign_out() {
    TOKEN.with(|t| *t.borrow_mut() = None);
    USER.with(|u| *u.borrow_mut() = None);
    if let Some(storage) = session_storage() {
        let _ = storage.remove_item(SESSION_KEY);
    }
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let _ = storage.clear();
        }
        let _ = window.location().reload();
    }
}

/// The only email domain a file may be sent to: the school's.
pub const SCHOOL_DOMAIN: &str = DOMAIN_HINT;

/// Hand in: share the saved file `id` with the teacher at `teacher` (read-only; Drive emails
/// them), and tell the student how it went.
pub fn send_to_teacher(id: &str, name: &str, teacher: &str) {
    let (id, name, teacher) = (id.to_string(), name.to_string(), teacher.to_string());
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            let token = access_token().await?;
            share_with_teacher(&token, &id, &name, &teacher).await
        }
        .await;
        match result {
            Ok(()) => notify(&format!("File inviato a {teacher}.\nÈ anche nel tuo Google Drive, cartella «{FOLDER_NAME}»: {name}")),
            Err(e) => notify(&format!("Il file è nel tuo Drive ma NON è stato inviato a {teacher}: {e}")),
        }
    });
}

/// Share the saved file with the teacher at `teacher` (read-only; Drive emails them).
async fn share_with_teacher(token: &str, file_id: &str, file_name: &str, teacher: &str) -> Result<(), String> {
    let student = request(token, "GET", &format!("{DRIVE_URL}/about?fields=user(displayName)"), None, None)
        .await
        .ok()
        .and_then(|answer| drivefmt::user_name(&answer))
        .unwrap_or_default();
    let message = encode(&drivefmt::share_message(&student, file_name));
    let url = format!("{FILES_URL}/{file_id}/permissions?sendNotificationEmail=true&emailMessage={message}&fields=id");
    let body = json!({ "type": "user", "role": "reader", "emailAddress": teacher });
    request(token, "POST", &url, Some("application/json"), Some(body.to_string().into_bytes())).await.map(|_| ())
}

/// The student's name from Drive, kept for the toolbar.
async fn remember_user(token: &str) {
    if USER.with(|u| u.borrow().is_some()) {
        return;
    }
    let answer = request(token, "GET", &format!("{DRIVE_URL}/about?fields=user(displayName)"), None, None).await;
    let name = answer.ok().and_then(|a| drivefmt::user_name(&a)).unwrap_or_else(|| "Google".to_string());
    USER.with(|u| *u.borrow_mut() = Some(name));
}

/// The web app's save hook: everything goes to Drive, with a link for Classroom, and falls back to
/// a download.
pub fn deliver(path: &str, bytes: &[u8]) -> Result<(), String> {
    let name = std::path::Path::new(path).file_name().map_or_else(|| "photoschool".to_string(), |n| n.to_string_lossy().to_string());
    if drive_disabled() || bytes.len() > MAX_UPLOAD_BYTES {
        return crate::web::download(&name, bytes);
    }
    let mime = crate::web::mime_for(&name).to_string();
    let bytes = bytes.to_vec();
    wasm_bindgen_futures::spawn_local(async move {
        match save(&name, &mime, &bytes).await {
            Ok(saved) => match link_for_school(&saved).await {
                Ok(link) => {
                    copy_to_clipboard(&link);
                    publish_link(&name, &link, &saved.folder, &saved.id);
                }
                Err(e) => notify(&format!("Salvato nel tuo Drive, cartella «{FOLDER_NAME}»: {name}. Il link non è pronto: {e}")),
            },
            Err(e) => {
                log::warn!("save to Drive failed: {e}");
                match crate::web::download(&name, &bytes) {
                    Ok(()) => notify(&format!("Non sono riuscito a salvare sul Drive ({e}). Il file è stato scaricato sul computer: {name}")),
                    Err(d) => notify(&format!("Salvataggio non riuscito. Drive: {e}. Download: {d}")),
                }
            }
        }
    });
    Ok(())
}

fn notify(message: &str) {
    NOTICE.with(|n| *n.borrow_mut() = Some(message.to_string()));
    wake();
}

pub fn drive_disabled() -> bool {
    let search = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    search.trim_start_matches('?').split('&').any(|p| p == "nodrive")
}

/// Try to put `text` on the clipboard. Browsers allow it only shortly after a click, so this can
/// quietly fail; the link window has a Copy button for that.
fn copy_to_clipboard(text: &str) {
    let Some(window) = web_sys::window() else { return };
    let navigator = window.navigator();
    let navigator: &JsValue = navigator.as_ref();
    let Some(clipboard) = field(navigator, "clipboard") else { return };
    let Some(write) = field(&clipboard, "writeText").and_then(|f| f.dyn_into::<js_sys::Function>().ok()) else { return };
    if let Ok(promise) = write.call1(&clipboard, &JsValue::from_str(text)) {
        // A refusal is a rejected promise: swallow it so it doesn't surface as an error.
        if let Ok(promise) = promise.dyn_into::<js_sys::Promise>() {
            wasm_bindgen_futures::spawn_local(async move {
                let _ = JsFuture::from(promise).await;
            });
        }
    }
}

/// Put `bytes` in the "Disegni" folder as `name`, replacing the file of that name if it is there.
async fn save(name: &str, mime: &str, bytes: &[u8]) -> Result<Saved, String> {
    let token = access_token().await?;
    save_with_token(token, name, mime, bytes).await
}

/// [`save`] with the access token already in hand.
async fn save_with_token(token: String, name: &str, mime: &str, bytes: &[u8]) -> Result<Saved, String> {
    let folder = find_or_create_folder(&token).await?;
    let existing = find_file(&token, &folder, name).await?;
    let (url, method, meta) = match existing {
        Some(id) => (format!("{UPLOAD_URL}/{id}?uploadType=multipart&fields=id,webViewLink"), "PATCH", json!({ "name": name })),
        None => (format!("{UPLOAD_URL}?uploadType=multipart&fields=id,webViewLink"), "POST", json!({ "name": name, "parents": [folder] })),
    };
    let (content_type, body) = drivefmt::multipart(&meta, mime, bytes);
    let answer = request(&token, method, &url, Some(&content_type), Some(body)).await?;
    let id = drivefmt::created_id(&answer).ok_or_else(|| "Drive non ha restituito il file salvato".to_string())?;
    Ok(Saved { token, id, link: drivefmt::web_link(&answer), folder: format!("https://drive.google.com/drive/folders/{folder}") })
}

/// Let everyone at the school open the file through its link (not search for it, not outside the
/// school), and return that link. A pasted link to a private file would not open for the teacher.
async fn link_for_school(saved: &Saved) -> Result<String, String> {
    let url = format!("{FILES_URL}/{}/permissions?fields=id", saved.id);
    let body = json!({ "type": "domain", "role": "reader", "domain": DOMAIN_HINT, "allowFileDiscovery": false });
    request(&saved.token, "POST", &url, Some("application/json"), Some(body.to_string().into_bytes())).await?;
    saved.link.clone().ok_or_else(|| "Drive non ha dato il link del file".to_string())
}

async fn find_or_create_folder(token: &str) -> Result<String, String> {
    let url = format!("{FILES_URL}?q={}&fields=files(id)&pageSize=1&spaces=drive", encode(&drivefmt::folder_query(FOLDER_NAME)));
    if let Some(id) = drivefmt::first_id(&request(token, "GET", &url, None, None).await?) {
        return Ok(id);
    }
    let meta = json!({ "name": FOLDER_NAME, "mimeType": drivefmt::FOLDER_MIME });
    let created = request(token, "POST", &format!("{FILES_URL}?fields=id"), Some("application/json"), Some(meta.to_string().into_bytes())).await?;
    drivefmt::created_id(&created).ok_or_else(|| "Drive non ha creato la cartella".to_string())
}

async fn find_file(token: &str, folder: &str, name: &str) -> Result<Option<String>, String> {
    let url = format!("{FILES_URL}?q={}&fields=files(id)&pageSize=1&spaces=drive", encode(&drivefmt::file_query(name, folder)));
    Ok(drivefmt::first_id(&request(token, "GET", &url, None, None).await?))
}

fn encode(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

/// One authorised call to the Drive API; the answer is its JSON body.
async fn request(token: &str, method: &str, url: &str, content_type: Option<&str>, body: Option<Vec<u8>>) -> Result<Value, String> {
    let headers = js_sys::Object::new();
    set(&headers, "Authorization", &JsValue::from_str(&format!("Bearer {token}")))?;
    if let Some(content_type) = content_type {
        set(&headers, "Content-Type", &JsValue::from_str(content_type))?;
    }
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_headers(&headers);
    if let Some(body) = body {
        init.set_body(&js_sys::Uint8Array::from(body.as_slice()));
    }
    let window = web_sys::window().ok_or("nessuna finestra del browser")?;
    let response = JsFuture::from(window.fetch_with_str_and_init(url, &init))
        .await
        .map_err(js_error)?
        .dyn_into::<web_sys::Response>()
        .map_err(|_| "risposta non valida da Google".to_string())?;
    let text = JsFuture::from(response.text().map_err(js_error)?).await.map_err(js_error)?.as_string().unwrap_or_default();
    if !response.ok() {
        return Err(format!("Drive ha risposto {}: {}", response.status(), drivefmt::short(&text, ERROR_TEXT_CHARS)));
    }
    serde_json::from_str(&text).map_err(|e| format!("risposta di Drive non leggibile: {e}"))
}

/// A valid access token: the one kept from earlier, or a new one after Google's sign-in popup.
async fn access_token() -> Result<String, String> {
    restore_session();
    let now = js_sys::Date::now();
    let kept = TOKEN.with(|t| t.borrow().as_ref().filter(|t| t.expires_at_ms - TOKEN_MARGIN_MS > now).map(|t| t.value.clone()));
    if let Some(value) = kept {
        return Ok(value);
    }
    load_identity_script().await?;
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        if let Err(e) = start_token_request(&resolve, &reject) {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&e));
        }
    });
    let response = JsFuture::from(promise).await.map_err(|e| e.as_string().unwrap_or_else(|| "accesso a Google non riuscito".to_string()))?;
    if let Some(error) = field(&response, "error").and_then(|e| e.as_string()) {
        return Err(format!("accesso a Google non riuscito: {error}"));
    }
    let value = field(&response, "access_token").and_then(|v| v.as_string()).ok_or("Google non ha dato il permesso di salvare")?;
    let seconds = field(&response, "expires_in").and_then(|v| v.as_f64().or_else(|| v.as_string().and_then(|s| s.parse().ok()))).unwrap_or(3000.0);
    let expires_at_ms = js_sys::Date::now() + seconds * 1000.0;
    TOKEN.with(|t| *t.borrow_mut() = Some(Token { value: value.clone(), expires_at_ms }));
    remember_user(&value).await;
    store_session();
    Ok(value)
}

/// Ask Google Identity Services for an access token; `resolve` gets its answer, `reject` a
/// closed or blocked popup.
fn start_token_request(resolve: &js_sys::Function, reject: &js_sys::Function) -> Result<(), String> {
    let window = web_sys::window().ok_or("nessuna finestra del browser")?;
    let oauth2 = path(window.as_ref(), &["google", "accounts", "oauth2"])?;
    let init = field(&oauth2, "initTokenClient")
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or("il login di Google non è disponibile")?;
    let on_token = {
        let resolve = resolve.clone();
        Closure::<dyn FnMut(JsValue)>::new(move |response: JsValue| {
            let _ = resolve.call1(&JsValue::NULL, &response);
        })
        .into_js_value()
    };
    let on_error = {
        let reject = reject.clone();
        Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
            let kind = field(&error, "type").and_then(|t| t.as_string()).unwrap_or_else(|| "errore".to_string());
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&format!("accesso a Google annullato ({kind})")));
        })
        .into_js_value()
    };
    let config = js_sys::Object::new();
    set(&config, "client_id", &JsValue::from_str(CLIENT_ID))?;
    set(&config, "scope", &JsValue::from_str(SCOPE))?;
    set(&config, "hd", &JsValue::from_str(DOMAIN_HINT))?;
    // Always let the student pick the account, so a shared computer never signs in by itself.
    set(&config, "prompt", &JsValue::from_str("select_account"))?;
    set(&config, "callback", &on_token)?;
    set(&config, "error_callback", &on_error)?;
    let client = init.call1(&oauth2, &config).map_err(js_error)?;
    let ask = field(&client, "requestAccessToken")
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or("il login di Google non è disponibile")?;
    ask.call0(&client).map_err(js_error)?;
    Ok(())
}

/// Load Google's sign-in script once.
async fn load_identity_script() -> Result<(), String> {
    let window = web_sys::window().ok_or("nessuna finestra del browser")?;
    if path(window.as_ref(), &["google", "accounts", "oauth2"]).is_ok() {
        return Ok(());
    }
    let document = window.document().ok_or("nessun documento del browser")?;
    let body = document.body().ok_or("la pagina non ha un corpo")?;
    let script = document.create_element("script").map_err(js_error)?;
    script.set_attribute("src", GIS_URL).map_err(js_error)?;
    script.set_attribute("async", "").map_err(js_error)?;
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let on_load = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let on_error = Closure::once_into_js(move || {
            let _ = reject.call0(&JsValue::NULL);
        });
        let _ = script.add_event_listener_with_callback("load", on_load.unchecked_ref());
        let _ = script.add_event_listener_with_callback("error", on_error.unchecked_ref());
    });
    body.append_child(&script).map_err(js_error)?;
    let loaded = JsFuture::from(promise).await;
    if loaded.is_err() {
        script.remove();
        return Err("impossibile caricare il login di Google (rete bloccata?)".to_string());
    }
    Ok(())
}

/// `root.a.b.c` for `keys` = `[a, b, c]`; an error when any step is missing.
fn path(root: &JsValue, keys: &[&str]) -> Result<JsValue, String> {
    let mut current = root.clone();
    for key in keys {
        current = field(&current, key).ok_or_else(|| format!("manca {key}"))?;
    }
    Ok(current)
}

/// The property `key` of `object`, or `None` when it is missing, `undefined` or `null`.
fn field(object: &JsValue, key: &str) -> Option<JsValue> {
    js_sys::Reflect::get(object, &JsValue::from_str(key)).ok().filter(|v| !v.is_undefined() && !v.is_null())
}

fn set(object: &js_sys::Object, key: &str, value: &JsValue) -> Result<(), String> {
    js_sys::Reflect::set(object, &JsValue::from_str(key), value).map(|_| ()).map_err(js_error)
}

fn js_error(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}
