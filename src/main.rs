//! codescape: explore a Git repository as a map of its source.
//!
//! Usage: codescape [REPO] [--3d] [--diff REV|RANGE] [--pr N] [--tour] [--loop]
//!                  [--trace ITF [--trace-rate N]] [--record DIR] [--record-fps N]
//!                  [--atlas N]

mod app;
mod scape;

// Re-exported at the crate root so the renderer can keep saying `crate::`.
pub use codescape::{atlas, camera, capture, diff, layout, overlay, scan, scene, tour, trace};

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("codescape: {message}");
    std::process::exit(2);
}

/// Why the Linux display is unusable, if it is. A Wayland compositor is enough
/// for in-process recording; X is only checked when Wayland is unavailable.
#[cfg(target_os = "linux")]
fn display_problem() -> Option<String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let Some(display) = std::env::var_os("DISPLAY") else {
        return Some("neither WAYLAND_DISPLAY nor DISPLAY is set".into());
    };
    display
        .is_empty()
        .then(|| "WAYLAND_DISPLAY and DISPLAY are both empty".into())
}

fn main() {
    // Makepad's Vulkan renderer does not expose texture readback at this pin.
    // Recording therefore selects its OpenGL renderer before Makepad starts;
    // ordinary interactive runs retain the default backend selection.
    #[cfg(target_os = "linux")]
    if std::env::args().any(|a| a == "--record" || a == "--shot") {
        std::env::set_var("MAKEPAD_GPU", "gl");
    }
    #[cfg(target_os = "linux")]
    if let Some(problem) = display_problem() {
        eprintln!("codescape: {problem}, so there is no display server to draw on.");
        eprintln!("  Log into a graphical session or provide a headless Wayland compositor.");
        eprintln!("  For headless work, the renderer-free benchmark needs no display:");
        eprintln!("    cargo run --release --no-default-features --bin bench -- .");
        std::process::exit(2);
    }
    app::app_main();
}
