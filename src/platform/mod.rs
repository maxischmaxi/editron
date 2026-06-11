//! Betriebssystem-Integration für „Datei öffnen mit Editron“.
//!
//! Linux/Windows liefern Doppelklick-Pfade als argv — dafür braucht es hier
//! nichts. macOS schickt stattdessen ein Apple Event (`odoc`), das der
//! cfg-gated Handler in `macos.rs` entgegennimmt; der Mainloop pollt die
//! aufgelaufenen Pfade über `take_opened_files()`.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{install_open_files_handler, take_opened_files};

#[cfg(not(target_os = "macos"))]
pub fn install_open_files_handler() {}

#[cfg(not(target_os = "macos"))]
pub fn take_opened_files() -> Vec<std::path::PathBuf> {
    Vec::new()
}
