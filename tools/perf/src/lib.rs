#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub exe: String,
    pub args: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Launch {
        targets: Vec<Target>,
        runs: usize,
        flush_standby: bool,
    },
    Typing {
        target: Target,
        keys: usize,
    },
}

pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mode = args.first().ok_or("expected launch or typing")?.as_str();
    let mut targets: Vec<Target> = Vec::new();
    let mut runs = None;
    let mut keys = None;
    let mut flush_standby = false;
    let mut i = 1;
    while i < args.len() {
        let option = args[i].as_str();
        if option == "--flush-standby" {
            flush_standby = true;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("missing value for {option}"))?;
        match option {
            "--exe" => targets.push(Target {
                exe: value.clone(),
                args: String::new(),
            }),
            "--args" => {
                targets
                    .last_mut()
                    .ok_or("--args needs a preceding --exe")?
                    .args = value.clone()
            }
            "--runs" => runs = Some(positive_count(value)?),
            "--keys" => keys = Some(positive_count(value)?),
            _ => return Err(format!("unknown option {option}")),
        }
        i += 2;
    }
    if targets.is_empty() {
        return Err("at least one --exe is required".into());
    }
    match mode {
        "launch" if keys.is_none() => Ok(Command::Launch {
            targets,
            runs: runs.ok_or("--runs is required")?,
            flush_standby,
        }),
        "typing" if runs.is_none() && !flush_standby && targets.len() == 1 => Ok(Command::Typing {
            target: targets.remove(0),
            keys: keys.ok_or("--keys is required")?,
        }),
        _ => Err("invalid mode or options for mode".into()),
    }
}

fn positive_count(value: &str) -> Result<usize, String> {
    let count = value
        .parse::<usize>()
        .map_err(|_| format!("invalid count: {value}"))?;
    if count == 0 {
        Err("count must be positive".into())
    } else {
        Ok(count)
    }
}

pub fn percentile(values: &[u64], percent: f64) -> Option<u64> {
    if values.is_empty() || !(0.0..=100.0).contains(&percent) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = ((percent / 100.0 * sorted.len() as f64).ceil() as usize).max(1);
    Some(sorted[rank - 1])
}

pub fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.into()
    }
}

pub fn quote_windows_arg(s: &str) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in s.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        if c == '"' {
            out.push_str(&"\\".repeat(backslashes * 2 + 1));
            out.push('"');
            backslashes = 0;
            continue;
        }
        out.push_str(&"\\".repeat(backslashes));
        backslashes = 0;
        out.push(c);
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

pub fn parse_launch_log(data: &str) -> Result<(i64, i64), String> {
    let (a, b) = data.trim().split_once(',').ok_or("invalid launch log")?;
    let a = a.parse::<i64>().map_err(|_| "invalid first QPC")?;
    let b = b.parse::<i64>().map_err(|_| "invalid second QPC")?;
    if b < a {
        return Err("launch log QPC order is reversed".into());
    }
    Ok((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn parse_interleaved_targets_and_warmup_count() {
        let command = parse_args(&words(&[
            "launch",
            "--exe",
            "a.exe",
            "--args",
            "--arm a",
            "--exe",
            "b.exe",
            "--runs",
            "3",
            "--flush-standby",
        ]))
        .unwrap();
        assert_eq!(
            command,
            Command::Launch {
                targets: vec![
                    Target {
                        exe: "a.exe".into(),
                        args: "--arm a".into()
                    },
                    Target {
                        exe: "b.exe".into(),
                        args: "".into()
                    }
                ],
                runs: 3,
                flush_standby: true
            }
        );
    }

    #[test]
    fn reject_missing_and_zero_counts() {
        assert!(parse_args(&words(&["launch", "--runs", "3"])).is_err());
        assert!(parse_args(&words(&["typing", "--exe", "a.exe", "--keys", "0"])).is_err());
        assert!(parse_args(&words(&["launch", "--exe", "a.exe", "--runs", "no"])).is_err());
    }

    #[test]
    fn nearest_rank_percentiles() {
        assert_eq!(percentile(&[4, 1, 3, 2], 50.0), Some(2));
        assert_eq!(percentile(&[4, 1, 3, 2], 95.0), Some(4));
        assert_eq!(percentile(&[], 95.0), None);
    }

    #[test]
    fn csv_quotes_special_fields() {
        assert_eq!(csv_field("simple"), "simple");
        assert_eq!(csv_field("a,\"b\n"), "\"a,\"\"b\n\"");
    }

    #[test]
    fn windows_argument_quoting_keeps_spaces_and_trailing_slashes() {
        assert_eq!(
            quote_windows_arg("C:\\Program Files\\"),
            "\"C:\\Program Files\\\\\""
        );
        assert_eq!(quote_windows_arg("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn launch_log_rejects_reversed_counters() {
        assert_eq!(parse_launch_log("10,20\n").unwrap(), (10, 20));
        assert!(parse_launch_log("20,10\n").is_err());
        assert!(parse_launch_log("bad\n").is_err());
    }
}
