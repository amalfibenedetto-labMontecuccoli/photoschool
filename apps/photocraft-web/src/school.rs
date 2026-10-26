//! The school's screens around the editor: the sign-in screen shown before anyone can work, the bar
//! with the student's name and the "Apri da Drive" / "Salva su Drive" buttons, and the windows
//! those open (file list, file name, link to paste into Classroom, messages).

use std::cell::RefCell;
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, RichText, vec2};
use photocraft_ui_egui::{FileDialogAnswer, FileDialogReply, PhotocraftApp};
use serde_json::json;

use crate::drive;

type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

const YELLOW: Color32 = Color32::from_rgb(250, 204, 21);
const ON_YELLOW: Color32 = Color32::from_rgb(40, 32, 0);

thread_local! {
    /// A save asked for a file name: the name suggested and where to send the answer.
    static NAME_REQUEST: RefCell<Option<(String, FileDialogReply)>> = const { RefCell::new(None) };
}

/// The browser has no save dialog: the app asks for the file name in our own window. Called when
/// the editor wants to save; the answer goes to `reply` once the student has typed the name.
pub fn ask_name(suggested: &str, reply: FileDialogReply) {
    NAME_REQUEST.with(|r| *r.borrow_mut() = Some((suggested.to_string(), reply)));
}

struct NameAsk {
    stem: String,
    extension: String,
    reply: FileDialogReply,
}

enum Listing {
    Loading,
    Files(Vec<(String, String, String)>),
    Failed(String),
}

#[derive(Default)]
pub struct School {
    name_ask: Option<NameAsk>,
    listing: Option<Listing>,
    /// The list is for looking at the files in Drive, not for opening one in the editor.
    browsing: bool,
    /// File name, link and folder link of what was just saved.
    link: Option<(String, String, String, String)>,
    /// The save was started by the Consegna button: the link window also offers to email the teacher.
    handing_in: bool,
    teacher_email: String,
    teacher_error: String,
    notice: Option<String>,
}

/// A file name without folders or characters Drive and Windows dislike.
fn clean_stem(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control()).collect();
    cleaned.trim().chars().take(80).collect()
}

impl School {
    /// Finished background work: files listed or downloaded, a link ready, a message, a save that
    /// needs a name. Called every frame.
    pub fn poll(&mut self, ctx: &egui::Context, inbox: &Inbox) {
        drive::remember_context(ctx);
        if let Some(listing) = drive::take_listing() {
            self.listing = Some(match listing {
                Ok(files) => Listing::Files(files),
                Err(e) => Listing::Failed(e),
            });
        }
        if let Some((name, bytes)) = drive::take_opened() {
            inbox.lock().unwrap_or_else(PoisonError::into_inner).push((name, bytes));
            ctx.request_repaint();
        }
        if let Some(link) = drive::take_link() {
            if self.handing_in {
                self.link = Some(link);
            } else {
                self.notice = Some(format!("Salvato nel tuo Google Drive, cartella «Photoschool»: {}", link.0));
            }
        }
        if let Some(notice) = drive::take_notice() {
            self.notice = Some(notice);
        }
        if let Some((suggested, reply)) = NAME_REQUEST.with(|r| r.borrow_mut().take()) {
            let path = std::path::Path::new(&suggested);
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let extension = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            // A request not yet answered counts as cancelled when a new one replaces it.
            self.name_ask = Some(NameAsk { stem: clean_stem(&stem), extension, reply });
        }
    }

    /// The screen shown until a student has signed in: nothing can be drawn before that, so no
    /// work is made that could not be saved.
    pub fn gate(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::from_rgb(38, 38, 38))).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 140.0).max(20.0));
                ui.label(RichText::new("Photoschool").size(40.0).strong());
                ui.add_space(8.0);
                ui.label(RichText::new("Per cominciare accedi con il tuo account della scuola.").size(18.0));
                ui.label(RichText::new("Il tuo lavoro si salva nel tuo Google Drive.").size(15.0).weak());
                ui.add_space(24.0);
                if ui.add(egui::Button::new(RichText::new("Accedi con Google").size(20.0).color(Color32::WHITE)).fill(Color32::from_rgb(66, 133, 244)).min_size(vec2(300.0, 56.0))).clicked() {
                    drive::sign_in();
                }
            });
        });
    }

    /// The bar above the editor: who is signed in, and the two Drive buttons.
    pub fn bar(&mut self, ui: &mut egui::Ui, app: &mut PhotocraftApp) {
        egui::Panel::top("school_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                // About the width of what follows, so the group sits near the middle.
                ui.add_space(((ui.available_width() - 900.0) / 2.0).max(8.0));
                let name = drive::account().unwrap_or_else(|| "Google".to_string());
                ui.label(RichText::new(name.chars().take(24).collect::<String>()).strong());
                if ui.button("Esci").on_hover_text("Esci dall'account e prepara il computer per il prossimo alunno").clicked() {
                    let sure = !app.has_unsaved_work()
                        || web_sys::window().and_then(|w| w.confirm_with_message("Ci sono modifiche non salvate: se esci si perdono. Vuoi uscire lo stesso?").ok()).unwrap_or(false);
                    if sure {
                        drive::sign_out();
                    }
                }
                ui.add_space(16.0);
                if ui.add(egui::Button::new(RichText::new("Apri da Drive").color(ON_YELLOW)).fill(YELLOW).min_size(vec2(150.0, 26.0))).clicked() {
                    self.browsing = false;
                    self.listing = Some(Listing::Loading);
                    drive::list_drive();
                }
                if ui.add(egui::Button::new(RichText::new("Salva su Drive").color(ON_YELLOW)).fill(YELLOW).min_size(vec2(150.0, 26.0))).clicked() {
                    self.handing_in = false;
                    let ctx = ui.ctx().clone();
                    match photocraft_ui_egui::menus::invoke(app, &ctx, "file.saveAs", json!({})) {
                        Ok(_) => {}
                        Err(e) if e == "cancelled" => {}
                        Err(e) => self.notice = Some(e),
                    }
                }
                if ui.add(egui::Button::new("Guida")).on_hover_text("Come si usa: accedere, salvare, consegnare").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab("guida.html"));
                }
                if ui.add(egui::Button::new("I miei file").min_size(vec2(110.0, 26.0))).on_hover_text("Guarda i file che hai salvato nel Drive").clicked() {
                    self.browsing = true;
                    self.listing = Some(Listing::Loading);
                    drive::list_drive();
                }
                if ui.add(egui::Button::new(RichText::new("Consegna").color(Color32::WHITE).strong()).fill(Color32::from_rgb(66, 133, 244)).min_size(vec2(150.0, 26.0))).on_hover_text("Salva il lavoro nel Drive e consegnalo al docente").clicked() {
                    self.handing_in = true;
                    self.teacher_error.clear();
                    let ctx = ui.ctx().clone();
                    match photocraft_ui_egui::menus::invoke(app, &ctx, "file.saveAs", json!({})) {
                        Ok(_) => {}
                        Err(e) if e == "cancelled" => {}
                        Err(e) => self.notice = Some(e),
                    }
                }
            });
        });
    }

    /// The windows over the editor. Also used on the sign-in screen, for its messages.
    pub fn windows(&mut self, ctx: &egui::Context, inbox: &Inbox) {
        self.notice_window(ctx);
        self.name_window(ctx);
        self.list_window(ctx, inbox);
        self.link_window(ctx);
    }

    fn notice_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.notice.clone() else { return };
        let mut close = false;
        egui::Window::new("Messaggio").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(&text);
            ui.add_space(8.0);
            if ui.button("OK").clicked() {
                close = true;
            }
        });
        if close {
            self.notice = None;
        }
    }

    fn name_window(&mut self, ctx: &egui::Context) {
        let Some(mut ask) = self.name_ask.take() else { return };
        let (mut save, mut cancel) = (false, false);
        egui::Window::new("Salva su Drive").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label("Nome del file:");
            ui.horizontal(|ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut ask.stem).desired_width(300.0));
                if !field.has_focus() && !field.lost_focus() {
                    field.request_focus();
                }
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                ui.label(&ask.extension);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Salva").size(15.0)).min_size(vec2(120.0, 30.0))).clicked() {
                    save = true;
                }
                if ui.button("Annulla").clicked() {
                    cancel = true;
                }
            });
        });
        let stem = clean_stem(&ask.stem);
        if cancel {
            ask.reply.send(None);
        } else if save && !stem.is_empty() {
            ask.reply.send(Some(FileDialogAnswer::SaveTo(format!("{stem}{}", ask.extension))));
        } else {
            self.name_ask = Some(ask);
        }
    }

    fn list_window(&mut self, ctx: &egui::Context, inbox: &Inbox) {
        let Some(listing) = &self.listing else { return };
        let mut close = false;
        let mut open: Option<(String, String)> = None;
        let browsing = self.browsing;
        let title = if browsing { "I miei file nel Drive" } else { "Apri da Drive" };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_min_width(380.0);
            match listing {
                Listing::Loading => {
                    ui.label("Cerco i tuoi file nel Drive…");
                }
                Listing::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(230, 80, 70), e);
                }
                Listing::Files(files) if files.is_empty() => {
                    ui.label("Non ho trovato file nella cartella «Photoschool» del tuo Drive.");
                }
                Listing::Files(files) => {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (id, name, modified) in files {
                            let label = if modified.is_empty() { name.clone() } else { format!("{name}   ({modified})") };
                            if browsing {
                                ui.horizontal(|ui| {
                                    ui.label(label);
                                    let link = format!("https://drive.google.com/file/d/{id}/view");
                                    if ui.button("Vedi nel Drive").clicked() {
                                        ctx.open_url(egui::OpenUrl::new_tab(link.clone()));
                                    }
                                    if ui.button("Copia link").clicked() {
                                        ctx.copy_text(link);
                                    }
                                });
                            } else if ui.add(egui::Button::new(label).min_size(vec2(380.0, 28.0))).clicked() {
                                open = Some((id.clone(), name.clone()));
                            }
                        }
                    });
                }
            }
            ui.add_space(8.0);
            if ui.button("Chiudi").clicked() {
                close = true;
            }
        });
        if let Some((id, name)) = open {
            drive::open_drive_file(&id, &name);
            close = true;
        }
        if close {
            self.listing = None;
        }
        let _ = inbox;
    }

    fn link_window(&mut self, ctx: &egui::Context) {
        let Some((name, link, folder, id)) = self.link.clone() else { return };
        let mut close = false;
        let mut send: Option<String> = None;
        let title = if self.handing_in { "Consegna il lavoro" } else { "Salvato nel Drive" };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.label(format!("Salvato nel tuo Google Drive: {name}"));
            ui.add_space(6.0);
            ui.label("Link da incollare in Classroom:");
            let mut shown = link.clone();
            ui.add(egui::TextEdit::singleline(&mut shown).desired_width(440.0));
            ui.add_space(4.0);
            if ui.add(egui::Button::new(RichText::new("Copia link").size(15.0)).min_size(vec2(140.0, 30.0))).clicked() {
                ctx.copy_text(link.clone());
            }
            ui.add_space(6.0);
            ui.label("In Classroom: apri il compito, scegli Aggiungi o crea, poi Link, e incolla.");
            ui.add(egui::Hyperlink::from_label_and_url("Apri la cartella nel Drive", folder).open_in_new_tab(true));
            if self.handing_in {
                ui.add_space(8.0);
                ui.separator();
                ui.label(format!("Oppure invialo per email a un docente (@{}):", drive::SCHOOL_DOMAIN));
                let field = ui.add(egui::TextEdit::singleline(&mut self.teacher_email).hint_text(format!("nome@{}", drive::SCHOOL_DOMAIN)).desired_width(440.0));
                if !self.teacher_error.is_empty() {
                    ui.colored_label(Color32::from_rgb(230, 80, 70), &self.teacher_error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Invia al docente").clicked() || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        send = Some(self.teacher_email.trim().to_ascii_lowercase());
                    }
                });
            }
            ui.add_space(6.0);
            if ui.button("Chiudi").clicked() {
                close = true;
            }
        });
        if let Some(email) = send {
            let suffix = format!("@{}", drive::SCHOOL_DOMAIN);
            let local = email.strip_suffix(&suffix).unwrap_or("");
            if local.is_empty() || local.contains('@') || local.chars().any(char::is_whitespace) {
                self.teacher_error = format!("Scrivi l'indirizzo scolastico del docente, che finisce con {suffix}");
            } else {
                self.teacher_error.clear();
                drive::send_to_teacher(&id, &name, &email);
                close = true;
            }
        }
        if close {
            self.link = None;
        }
    }
}
