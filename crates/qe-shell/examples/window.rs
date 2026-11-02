//! A visible window on the real renderer, for root's visible checks. Tests never run it.
use qe_api::{InputKind, Renderer};
use qe_render::BitmapRenderer;
use qe_shell::{ShellEvent, ShellWindow, Tab, quit_message_loop, run_message_loop};

#[path = "../tests/support/mod.rs"]
mod support;

const VK_TAB: u16 = 0x09;

fn main() -> Result<(), String> {
    let renderer: Box<dyn Renderer> =
        Box::new(BitmapRenderer::new(800, 600).map_err(|e| e.to_string())?);
    let window = ShellWindow::create(renderer, Box::new(support::RealMeasurer), 8.0, 17.0)
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
