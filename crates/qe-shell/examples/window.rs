//! A visible window on the real renderer, for root's visible checks. Tests never run it.
use qe_api::InputKind;
use qe_render::BitmapRenderer;
use qe_shell::{ShellEvent, ShellWindow, Tab, WindowConfig, quit_message_loop, run_message_loop};
use windows::Win32::Foundation::HWND;

#[path = "../tests/support/mod.rs"]
mod support;

const VK_TAB: u16 = 0x09;

fn main() -> Result<(), String> {
    let window = ShellWindow::create_with_refresh(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            ..Default::default()
        },
        BitmapRenderer::new(800, 600).map_err(|e| e.to_string())?,
        |renderer, hwnd| renderer.refresh_text_settings_for_window(HWND(hwnd)),
        Box::new(support::RealMeasurer),
    )
    .map_err(|e| e.to_string())?;
    window
        .add_tab(Tab::new("Example.rs"))
        .map_err(|e| e.to_string())?;
    window
        .add_tab(Tab::new("A longer tab name"))
        .map_err(|e| e.to_string())?;
    window.set_event_handler(|handle, event| {
        let result = match event {
            ShellEvent::NewTab => handle.add_tab(Tab::new("Untitled")).map(|_| ()),
            ShellEvent::CloseTabRequested(id) => handle.close_tab(id),
            ShellEvent::CloseWindowRequested => handle.destroy(),
            ShellEvent::WindowDestroyed => {
                quit_message_loop(0);
                Ok(())
            }
            ShellEvent::ContentInput(input) => match input.kind {
                // There is no text area yet, so Tab moves focus into the strip.
                InputKind::KeyDown {
                    virtual_key: VK_TAB,
                    ..
                } => handle.focus_strip(),
                _ => Ok(()),
            },
            ShellEvent::Problem(problem) => {
                eprintln!("{problem}");
                Ok(())
            }
            other => {
                println!("{other:?}");
                Ok(())
            }
        };
        if let Err(problem) = result {
            eprintln!("{problem}");
        }
    });
    window.show().map_err(|e| e.to_string())?;
    run_message_loop().map_err(|e| e.to_string())?;
    Ok(())
}
