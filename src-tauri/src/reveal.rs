//! Datei im Dateimanager des Systems anzeigen.

use std::path::Path;
use std::process::Command;

/// Prozent-Encoding für file://-URLs (alles außer unreserved + "/").
#[cfg(all(unix, not(target_os = "macos")))]
fn encode_file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char);
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Zeigt `path` im Dateimanager an: macOS selektiert im Finder, Windows im
/// Explorer; unter Linux wird zuerst das FileManager1-D-Bus-Interface
/// versucht (selektiert die Datei), sonst der Ordner per xdg-open geöffnet.
pub fn reveal_in_file_manager(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("Pfad existiert nicht: {path}"));
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-R", path])
            .spawn()
            .map_err(|e| format!("Finder konnte nicht gestartet werden: {e}"))?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
            .map_err(|e| format!("Explorer konnte nicht gestartet werden: {e}"))?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let url = encode_file_url(path);
        let dbus = Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{url}"),
                "string:",
            ])
            .status();
        if matches!(dbus, Ok(status) if status.success()) {
            return Ok(());
        }
        let dir = p.parent().unwrap_or_else(|| Path::new("/"));
        Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map_err(|e| format!("Dateimanager konnte nicht gestartet werden: {e}"))?;
        Ok(())
    }
}
