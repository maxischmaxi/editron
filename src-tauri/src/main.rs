#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // webkit2gtk stürzt mit dem proprietären NVIDIA-Treiber unter Wayland ab
    // (DMA-BUF-Renderer, GDK "Error 71 dispatching to Wayland display").
    // Workaround nur setzen, wenn der Nutzer nichts Eigenes vorgegeben hat.
    #[cfg(target_os = "linux")]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let nvidia = std::path::Path::new("/proc/driver/nvidia/version").exists();
        if wayland && nvidia && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    editron_lib::run()
}
