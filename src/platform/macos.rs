//! macOS: „Öffnen mit“/Doppelklick liefert keine argv, sondern ein
//! Apple Event (`aevt`/`odoc`). Hier wird nach der Fenster-Initialisierung
//! ein Handler am NSAppleEventManager registriert, der die Datei-URLs in
//! eine globale Queue legt; der Mainloop holt sie über `take_opened_files()`.
//!
//! Hinweis: auf macOS bisher nicht verifiziert (entwickelt unter Linux) —
//! der Code ist bewusst klein und isoliert gehalten.

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use std::path::PathBuf;
use std::sync::Mutex;

static OPENED_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

// FourCharCodes: kCoreEventClass / kAEOpenDocuments / keyDirectObject / typeFileURL
const K_CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const K_AE_OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_FILE_URL: u32 = u32::from_be_bytes(*b"furl");

/// Vom AppleEventManager aufgerufen: Datei-URLs aus dem Event ziehen.
extern "C" fn handle_open_documents(
    _this: &Object,
    _sel: Sel,
    event: *mut Object,
    _reply: *mut Object,
) {
    if event.is_null() {
        return;
    }
    unsafe {
        let direct: *mut Object = msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        if direct.is_null() {
            return;
        }
        let count: isize = msg_send![direct, numberOfItems];
        // numberOfItems == 0: Deskriptor ist keine Liste, sondern direkt ein Wert.
        let indices: Vec<isize> = if count > 0 { (1..=count).collect() } else { vec![0] };
        for i in indices {
            let item: *mut Object = if i == 0 {
                direct
            } else {
                msg_send![direct, descriptorAtIndex: i]
            };
            if item.is_null() {
                continue;
            }
            let coerced: *mut Object = msg_send![item, coerceToDescriptorType: TYPE_FILE_URL];
            if coerced.is_null() {
                continue;
            }
            let data: *mut Object = msg_send![coerced, data];
            if data.is_null() {
                continue;
            }
            let url: *mut Object =
                msg_send![class!(NSURL), URLWithDataRepresentation: data relativeToURL: std::ptr::null_mut::<Object>()];
            if url.is_null() {
                continue;
            }
            let path_ns: *mut Object = msg_send![url, path];
            if path_ns.is_null() {
                continue;
            }
            let utf8: *const std::os::raw::c_char = msg_send![path_ns, UTF8String];
            if utf8.is_null() {
                continue;
            }
            let path = std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned();
            OPENED_FILES.lock().unwrap().push(PathBuf::from(path));
        }
    }
}

/// Handler-Klasse registrieren und am NSAppleEventManager anmelden.
/// Nach der raylib/GLFW-Initialisierung aufrufen (NSApplication existiert).
pub fn install_open_files_handler() {
    unsafe {
        let class_name = "EditronAppleEventHandler";
        let handler_class: &Class = match Class::get(class_name) {
            Some(c) => c,
            None => {
                let superclass = class!(NSObject);
                let mut decl = match ClassDecl::new(class_name, superclass) {
                    Some(d) => d,
                    None => return,
                };
                decl.add_method(
                    sel!(handleOpenDocuments:withReplyEvent:),
                    handle_open_documents
                        as extern "C" fn(&Object, Sel, *mut Object, *mut Object),
                );
                decl.register()
            }
        };
        let handler: *mut Object = msg_send![handler_class, new];
        let manager: *mut Object = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let _: () = msg_send![manager,
            setEventHandler: handler
            andSelector: sel!(handleOpenDocuments:withReplyEvent:)
            forEventClass: K_CORE_EVENT_CLASS
            andEventID: K_AE_OPEN_DOCUMENTS];
        // handler lebt absichtlich für die Prozesslaufzeit (kein release).
    }
}

/// Aufgelaufene Doppelklick-Pfade abholen (Mainloop, pro Frame).
pub fn take_opened_files() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED_FILES.lock().unwrap())
}
