//! Every tab and state through the real renderer. Saved images are compared by pixels.
//! Update them with `QE_SETTINGS_UPDATE_IMAGES=1`, then look at each changed picture.
#[path = "../../../qe-ui/tests/support/png.rs"]
mod png;

use qe_api::{
    Bitmap, CommandId, Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, Rect,
    Renderer, SettingKey, TextMeasurer, Theme, ThemeKind, View, ViewContext, WidgetId,
};
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};
use qe_settings::*;
use qe_ui::{SystemPalette, UiTokens};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

fn image_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("images")
}
fn fresh_store(name: &str) -> SettingsStore {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("qe-settings-pictures")
        .join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    SettingsStore::open(folder.join("settings.txt"))
}
struct Scene {
    panel: SettingsPanel,
    ctx: ViewContext,
    measure: DirectWriteLayout,
    store: SettingsStore,
}
impl Scene {
    fn new(name: &str, tab: usize) -> Self {
        Self::with_store(fresh_store(name), tab)
    }
    fn with_store(store: SettingsStore, tab: usize) -> Self {
        let mut scene = Self {
            panel: SettingsPanel::new(WidgetId(0x1000), store.clone()),
            ctx: ViewContext::default(),
            measure: DirectWriteLayout::new().unwrap(),
            store,
        };
        scene.panel.select_tab(tab, &mut scene.ctx);
        scene.panel.fit_to_text(&scene.measure);
        scene.lay_out(scene.panel.wanted_height());
        scene
    }
    fn lay_out(&mut self, height: f32) {
        self.panel.layout(
            Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(SETTINGS_WIDTH),
                height: Dip(height),
            },
            &self.measure,
        );
    }
    fn down(&mut self, virtual_key: u16, modifiers: Modifiers) {
        let event = InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key,
                modifiers,
                repeat: false,
            },
        };
        self.panel.handle(&event, &mut self.ctx);
    }
    fn tab_to(&mut self, name: &str) {
        for _ in 0..64 {
            if self.panel.focused_name().as_deref() == Some(name) {
                return;
            }
            self.down(0x09, Modifiers::default());
        }
        panic!("Tab never reached {name}");
    }
    fn paint(&self, theme: &Theme, dpi: u32) -> Bitmap {
        let height = self.panel.wanted_height();
        let scale = dpi as f32 / 96.0;
        let (width, pixels_high) = (
            (SETTINGS_WIDTH * scale).round() as u32,
            (height * scale).round() as u32,
        );
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(SETTINGS_WIDTH),
                height: Dip(height),
            },
            color: UiTokens::from_theme(theme).background,
        });
        self.panel.draw(&mut list, theme, &self.measure);
        let mut renderer = BitmapRenderer::new(width, pixels_high).unwrap();
        renderer
            .set_text_settings(TextSettings::reference())
            .unwrap();
        renderer.set_dpi(dpi).unwrap();
        renderer
            .render_to_bitmap(&list, width, pixels_high)
            .unwrap_or_else(|e| panic!("the renderer refused a command: {e}"))
    }
}
fn check(name: &str, image: &Bitmap) {
    let path = image_dir().join(format!("{name}.png"));
    if std::env::var_os("QE_SETTINGS_UPDATE_IMAGES").is_some() {
        std::fs::create_dir_all(image_dir()).unwrap();
        std::fs::write(&path, png::encode(image)).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (w, h, saved) = png::decode(&bytes).unwrap();
    assert_eq!((w, h), (image.width, image.height), "{}", path.display());
    let current = png::raw_rows(image);
    assert_eq!(saved.len(), current.len(), "{}", path.display());
    let different = saved
        .iter()
        .zip(&current)
        .filter(|(a, b)| a.abs_diff(**b) > 3)
        .count();
    assert_eq!(
        different,
        0,
        "{} differs in {different} channels",
        path.display()
    );
}
const LIGHT: Theme = Theme::new(ThemeKind::Light);
const DARK: Theme = Theme::new(ThemeKind::Dark);
fn high_contrast() -> Theme {
    SystemPalette::HIGH_CONTRAST_BLACK.theme()
}

#[test]
fn all_three_tabs_in_light_and_dark_at_100_150_and_200_percent() {
    for (tab, tab_name) in [
        (TAB_NEW_DOCUMENT, "new-document"),
        (TAB_OPEN_AND_SAVE, "open-and-save"),
        (TAB_KEYBOARD, "keyboard"),
    ] {
        let scene = Scene::new(&format!("tabs-{tab_name}"), tab);
        for (theme, theme_name) in [(LIGHT, "light"), (DARK, "dark")] {
            for dpi in [96, 144, 192] {
                let image = scene.paint(&theme, dpi);
                check(&format!("{tab_name}-{theme_name}-{dpi}"), &image);
            }
        }
    }
}
#[test]
fn high_contrast_on_all_three_tabs() {
    for (tab, tab_name) in [
        (TAB_NEW_DOCUMENT, "new-document"),
        (TAB_OPEN_AND_SAVE, "open-and-save"),
        (TAB_KEYBOARD, "keyboard"),
    ] {
        let scene = Scene::new(&format!("contrast-{tab_name}"), tab);
        let theme = SystemPalette::HIGH_CONTRAST_BLACK.theme();
        check(&format!("{tab_name}-contrast-96"), &scene.paint(&theme, 96));
    }
}
fn keyboard_recording(name: &str) -> Scene {
    let mut scene = Scene::new(name, TAB_KEYBOARD);
    scene.tab_to("Keyboard shortcuts");
    for _ in 0..4 {
        scene.down(0x28, Modifiers::default());
    }
    scene.down(0x0d, Modifiers::default());
    assert!(scene.panel.table().unwrap().is_recording());
    scene
}
#[test]
fn the_recording_state_a_held_modifier_and_a_clash_message() {
    let mut scene = keyboard_recording("recording");
    for (theme, theme_name) in [
        (LIGHT, "light"),
        (DARK, "dark"),
        (high_contrast(), "contrast"),
    ] {
        check(
            &format!("recording-{theme_name}-96"),
            &scene.paint(&theme, 96),
        );
    }
    let ctrl = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    scene.down(0x11, ctrl);
    check("recording-held-light-96", &scene.paint(&LIGHT, 96));
    check(
        "recording-held-contrast-96",
        &scene.paint(&high_contrast(), 96),
    );
    scene.down(0x46, ctrl);
    assert!(scene.panel.key_message().contains("already used"));
    for (theme, theme_name) in [(LIGHT, "light"), (DARK, "dark")] {
        check(&format!("clash-{theme_name}-96"), &scene.paint(&theme, 96));
    }
    let contrast = SystemPalette::HIGH_CONTRAST_BLACK.theme();
    check("clash-contrast-96", &scene.paint(&contrast, 96));
}
#[test]
fn changed_rows_show_the_marker_and_the_arrow() {
    let mut scene = Scene::new("changed", TAB_KEYBOARD);
    scene.store.clear_key(CommandId::Open);
    scene
        .store
        .set_key(
            CommandId::Rename,
            qe_api::KeyBinding::new(0x47, true, false, true),
        )
        .unwrap();
    scene.tab_to("Keyboard shortcuts");
    for (theme, theme_name) in [(LIGHT, "light"), (DARK, "dark")] {
        check(
            &format!("changed-{theme_name}-96"),
            &scene.paint(&theme, 96),
        );
    }
    let contrast = SystemPalette::HIGH_CONTRAST_BLACK.theme();
    check("changed-contrast-96", &scene.paint(&contrast, 96));
}
#[test]
fn a_disabled_control_a_refused_size_and_a_write_error() {
    let mut scene = Scene::new("disabled", TAB_NEW_DOCUMENT);
    scene.panel.disable_for_picture("Change\u{2026}");
    scene.panel.disable_for_picture("Smart quotes");
    for (theme, theme_name) in [(LIGHT, "light"), (DARK, "dark")] {
        check(
            &format!("disabled-{theme_name}-96"),
            &scene.paint(&theme, 96),
        );
    }
    let contrast = SystemPalette::HIGH_CONTRAST_BLACK.theme();
    check("disabled-contrast-96", &scene.paint(&contrast, 96));

    let mut scene = Scene::new("refused-size", TAB_NEW_DOCUMENT);
    let event = InputEvent {
        time_us: 0,
        kind: InputKind::Char('0'),
    };
    scene.panel.handle(&event, &mut scene.ctx);
    scene.down(0x0d, Modifiers::default());
    assert!(scene.panel.size_message().starts_with("Width must"));
    check("refused-size-light-96", &scene.paint(&LIGHT, 96));
    check(
        "refused-size-contrast-96",
        &scene.paint(&high_contrast(), 96),
    );

    let mut scene = Scene::new("write-error", TAB_NEW_DOCUMENT);
    std::fs::write(scene.store.path(), "").unwrap();
    let mut permissions = std::fs::metadata(scene.store.path()).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(scene.store.path(), permissions.clone()).unwrap();
    scene.store.set_bool(SettingKey::SmartQuotes, true).unwrap();
    assert!(scene.store.flush().is_err());
    scene.panel.refresh();
    check("write-error-light-96", &scene.paint(&LIGHT, 96));
    check(
        "write-error-contrast-96",
        &scene.paint(&high_contrast(), 96),
    );
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(scene.store.path(), permissions).unwrap();
}
#[test]
fn an_open_encoding_list_stays_inside_the_window() {
    let mut scene = Scene::new("open-list", TAB_OPEN_AND_SAVE);
    scene.tab_to("Encoding for opening files");
    scene.down(0x20, Modifiers::default());
    check("open-list-light-96", &scene.paint(&LIGHT, 96));
    check("open-list-dark-96", &scene.paint(&DARK, 96));
    check("open-list-contrast-96", &scene.paint(&high_contrast(), 96));
}
/// The message holds a path, so this store's path is relative: the picture is the same in
/// every clone. The folder is under `target/`, which Git ignores.
fn broken_store(name: &str, text: &str) -> (SettingsStore, PathBuf) {
    let folder = PathBuf::from("target")
        .join("qe-settings-pictures")
        .join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("settings.txt"), text).unwrap();
    (SettingsStore::open(folder.join("settings.txt")), folder)
}
#[test]
fn the_whole_load_problem_shows_in_a_taller_footer() {
    let (store, folder) = broken_store(
        "load-problem",
        "this line is junk
",
    );
    let problem = store.load_problem().unwrap();
    assert!(problem.contains("settings.broken.txt"), "{problem}");
    let scene = Scene::with_store(store.clone(), TAB_NEW_DOCUMENT);
    assert!(scene.panel.wanted_height() > TEXT_TABS_HEIGHT);
    for (theme, theme_name) in [
        (LIGHT, "light"),
        (DARK, "dark"),
        (high_contrast(), "contrast"),
    ] {
        check(
            &format!("load-problem-{theme_name}-96"),
            &scene.paint(&theme, 96),
        );
    }
    check("load-problem-light-144", &scene.paint(&LIGHT, 144));
    let scene = Scene::with_store(store, TAB_KEYBOARD);
    check("load-problem-keyboard-light-96", &scene.paint(&LIGHT, 96));
    drop(scene);
    std::fs::remove_dir_all(folder).unwrap();
}
#[test]
fn long_keys_grow_to_the_left_and_stay_different() {
    let store = fresh_store("long-keys");
    let key = |code, ctrl, alt, shift| qe_api::KeyBinding::new(code, ctrl, alt, shift);
    store
        .set_key(CommandId::New, key(0x21, true, true, true))
        .unwrap();
    store
        .set_key(CommandId::Open, key(0x22, true, true, true))
        .unwrap();
    store
        .set_key(CommandId::Close, key(0x7b, true, true, true))
        .unwrap();
    store
        .set_key(CommandId::Save, key(0x22, true, false, true))
        .unwrap();
    store
        .set_key(CommandId::Rename, key(0x6b, true, true, true))
        .unwrap();
    let scene = Scene::with_store(store, TAB_KEYBOARD);
    for (theme, theme_name) in [
        (LIGHT, "light"),
        (DARK, "dark"),
        (high_contrast(), "contrast"),
    ] {
        check(
            &format!("long-keys-{theme_name}-96"),
            &scene.paint(&theme, 96),
        );
    }
}
#[test]
fn a_long_font_name_wraps_and_a_longer_one_gets_an_ellipsis() {
    let store = fresh_store("long-font");
    store
        .set_font(&FontChoice {
            family: "Cascadia Mono ExtraLight Semi Condensed".into(),
            points: 10.5,
        })
        .unwrap();
    let scene = Scene::with_store(store, TAB_NEW_DOCUMENT);
    check("long-font-light-96", &scene.paint(&LIGHT, 96));
    let store = fresh_store("longer-font");
    store
        .set_font(&FontChoice {
            family: "A Very Long Family Name That No Font Dialog Would Ever Give "
                .repeat(3)
                .trim()
                .to_owned(),
            points: 10.5,
        })
        .unwrap();
    let scene = Scene::with_store(store, TAB_NEW_DOCUMENT);
    check("longer-font-light-96", &scene.paint(&LIGHT, 96));
}

struct RealHost;
impl SettingsHost for RealHost {
    fn measurer(&self) -> Arc<dyn TextMeasurer> {
        Arc::new(DirectWriteLayout::new().unwrap())
    }
    fn theme(&self) -> Theme {
        Theme::new(ThemeKind::Dark)
    }
    fn make_renderer(
        &self,
        width: u32,
        height: u32,
        dpi: u32,
    ) -> Result<Box<dyn Renderer>, String> {
        let mut renderer = BitmapRenderer::new(width, height).map_err(|e| e.to_string())?;
        renderer.set_dpi(dpi).map_err(|e| e.to_string())?;
        Ok(Box::new(renderer))
    }
}
/// The window with the real renderer, hidden: it paints with no problem to report.
#[test]
fn the_hidden_window_paints_with_the_real_renderer() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    let store = fresh_store("real-host");
    let mut window = SettingsWindow::new(store, Rc::new(RealHost));
    window.open_as(Presentation::Hidden).unwrap();
    let hwnd = window.hwnd().unwrap();
    // SAFETY: the window belongs to this thread, and it stays hidden.
    unsafe {
        assert!(!IsWindowVisible(hwnd).as_bool());
        SendMessageW(hwnd, WM_PAINT, Some(WPARAM(0)), Some(LPARAM(0)));
        SendMessageW(hwnd, WM_KEYDOWN, Some(WPARAM(0x09)), Some(LPARAM(1)));
    }
    assert_eq!(window.with_panel(|p| p.status_text()).as_deref(), Some(""));
}
