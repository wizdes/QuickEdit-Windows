//! The command line. Every flag that is not a file keeps the window off the screen, or measures.
use std::path::PathBuf;

const HELP: &str = "\
QuickEdit [options] [files]
  --hidden-smoke     hidden window: first frame, then exit
  --hidden           hidden windows for the whole run
  --test-channel     read test commands from stdin, implies hidden windows
  --stub-spell       spelling uses a small test checker, never the Windows one
  --data-root DIR    settings, session and pipe name under this folder
  --bench-keys N     hidden window: sends N keys and prints the timers
  --bench-middle     with --bench-keys: the keys go to the middle of the text
  --timings          prints the timers when the app ends, with real keys
  --watchdog MS      test channel: ends with code 99 if the UI thread stops answering
  --test-fail-windows N  the next N windows fail to open, for tests
  --help             prints this and opens nothing";

#[derive(Debug, Default)]
pub struct Args {
    pub files: Vec<PathBuf>,
    pub hidden: bool,
    pub smoke: bool,
    pub channel: bool,
    pub stub_spell: bool,
    pub root: Option<PathBuf>,
    pub timings: bool,
    pub bench_keys: usize,
    pub bench_middle: bool,
    pub help: bool,
    pub watchdog_ms: u64,
    pub fail_windows: usize,
}

impl Args {
    pub fn help_text() -> &'static str {
        HELP
    }
    pub fn parse(mut items: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut args = Self::default();
        while let Some(item) = items.next() {
            match item.to_str() {
                Some("--help") => args.help = true,
                Some("--hidden") => args.hidden = true,
                Some("--hidden-smoke") => {
                    args.hidden = true;
                    args.smoke = true;
                }
                Some("--test-channel") => {
                    args.hidden = true;
                    args.channel = true;
                }
                Some("--stub-spell") => args.stub_spell = true,
                Some("--timings") => args.timings = true,
                Some("--bench-middle") => args.bench_middle = true,
                Some("--bench-keys") => {
                    args.bench_keys = items
                        .next()
                        .and_then(|n| n.to_str().and_then(|n| n.parse().ok()))
                        .ok_or("--bench-keys needs a number")?;
                    args.hidden = true;
                    args.timings = true;
                }
                Some("--watchdog") => {
                    args.watchdog_ms = items
                        .next()
                        .and_then(|n| n.to_str().and_then(|n| n.parse().ok()))
                        .ok_or("--watchdog needs a number")?;
                }
                Some("--test-fail-windows") => {
                    args.fail_windows = items
                        .next()
                        .and_then(|n| n.to_str().and_then(|n| n.parse().ok()))
                        .ok_or("--test-fail-windows needs a number")?;
                }
                Some("--data-root") => {
                    args.root = Some(PathBuf::from(
                        items.next().ok_or("--data-root needs a folder")?,
                    ));
                }
                // A mistyped flag must not open a window on screen.
                Some(flag) if flag.starts_with("--") => {
                    return Err(format!("unknown option {flag}\n{HELP}"));
                }
                _ => args.files.push(PathBuf::from(item)),
            }
        }
        if args.bench_middle && args.bench_keys == 0 {
            return Err(format!("--bench-middle needs --bench-keys\n{HELP}"));
        }
        Ok(args)
    }
}
