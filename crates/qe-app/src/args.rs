//! The command line. Every flag that is not a file keeps the window off the screen, or measures.
//!
//! The flags are for tests and for measuring, so they work only in a copy with no package. In
//! the Store's package every argument is a file: Windows puts paths there, and the path may be
//! any name at all. See [`Args::parse`].
use std::ffi::OsString;
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
  --print-packaged-args ARGS  prints what a packaged run reads ARGS as, and opens nothing
  --help             prints this and opens nothing
  --                 everything after this is a file, whatever its name

A file whose name starts with -- also opens when given by its full path, or after --.
In the Store's package there are no options, and every argument is a file.";

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
    /// `--print-packaged-args`: what a packaged run would read the rest of the line as.
    pub packaged_view: Option<Vec<PathBuf>>,
}

impl Args {
    pub fn help_text() -> &'static str {
        HELP
    }
    /// `packaged` is whether this process has a package identity. There, no argument is a flag.
    pub fn parse(items: impl Iterator<Item = OsString>, packaged: bool) -> Result<Self, String> {
        if packaged {
            Ok(Self::all_files(items))
        } else {
            Self::parse_flags(items)
        }
    }
    fn all_files(items: impl Iterator<Item = OsString>) -> Self {
        Self {
            files: items.map(PathBuf::from).collect(),
            ..Self::default()
        }
    }
    fn parse_flags(mut items: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut args = Self::default();
        while let Some(item) = items.next() {
            match item.to_str() {
                Some("--") => {
                    args.files.extend(items.by_ref().map(PathBuf::from));
                }
                Some("--print-packaged-args") => {
                    args.packaged_view = Some(Self::all_files(items.by_ref()).files);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn items(list: &[&str]) -> impl Iterator<Item = OsString> {
        list.iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn a_packaged_run_reads_every_argument_as_a_file() {
        let line = [
            "--help",
            "--hidden",
            "--test-channel",
            "--data-root",
            r"\\host\share",
            "--no-such-flag",
            "--",
            "plain.txt",
        ];
        let args = Args::parse(items(&line), true).unwrap();
        let files: Vec<PathBuf> = line.iter().map(PathBuf::from).collect();
        assert_eq!(args.files, files);
        assert!(!args.help && !args.hidden && !args.channel && !args.smoke);
        assert!(args.root.is_none());
    }

    #[test]
    fn an_unpackaged_run_keeps_its_flags_and_names_a_bad_one() {
        let args = Args::parse(items(&["--hidden", "--stub-spell", "a.txt"]), false).unwrap();
        assert!(args.hidden && args.stub_spell);
        assert_eq!(args.files, [PathBuf::from("a.txt")]);
        let bad = Args::parse(items(&["--no-such-flag"]), false).unwrap_err();
        assert!(bad.contains("unknown option --no-such-flag"), "{bad}");
    }

    #[test]
    fn two_dashes_end_the_flags_and_the_rest_are_files() {
        let args = Args::parse(items(&["--hidden", "--", "--help", "--", "--x.txt"]), false);
        let args = args.unwrap();
        assert!(args.hidden && !args.help);
        assert_eq!(
            args.files,
            [
                PathBuf::from("--help"),
                PathBuf::from("--"),
                PathBuf::from("--x.txt")
            ]
        );
        assert!(Args::parse(items(&["--"]), false).unwrap().files.is_empty());
    }

    #[test]
    fn a_full_path_to_a_file_named_like_a_flag_is_a_file() {
        let path = r"C:\some folder\--help";
        let args = Args::parse(items(&[path]), false).unwrap();
        assert!(!args.help);
        assert_eq!(args.files, [PathBuf::from(path)]);
    }
}
