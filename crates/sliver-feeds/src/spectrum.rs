//! The `cava` module's spectrum: the `cava` program with raw output, run only while a spectrum
//! is on the lit bar (the agent decides, [`crate::Feeds::run_spectrum`]), so an idle, dark or
//! spectrum-free bar costs nothing (invariant 3).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use sliver_proc::ChildGuard;

use crate::{Feed, Out};

/// One frame of cava's raw ASCII output: levels 0–1000 separated by spaces, as 0..=1.
pub(crate) fn parse(line: &str) -> Vec<f32> {
    line.split_ascii_whitespace()
        .filter_map(|w| w.parse::<u16>().ok())
        .map(|v| f32::from(v.min(1000)) / 1000.0)
        .collect()
}

/// Starts `cava` with `config` (given on its stdin, so nothing is written to disk) and forwards
/// each frame that differs from the last: a silent spectrum sends nothing, so the bar doesn't
/// redraw. When cava ends, an empty spectrum follows.
pub(crate) fn start(program: &str, config: &str, out: Out) -> std::io::Result<ChildGuard> {
    let mut cmd = Command::new(program);
    cmd.args(["-p", "/dev/stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = ChildGuard::new(sliver_proc::spawn_tied(cmd)?);
    // The config is far smaller than a pipe buffer; dropping the handle ends cava's read.
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(config.as_bytes())?;
    let stdout = child.stdout.take().expect("piped");
    std::thread::Builder::new()
        .name("feed spectrum".into())
        .spawn(move || {
            let mut last = Vec::new();
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let levels = parse(&line);
                if levels != last {
                    last.clone_from(&levels);
                    if !out.send(Feed::Spectrum(levels)) {
                        return;
                    }
                }
            }
            out.send(Feed::Spectrum(Vec::new()));
        })?;
    Ok(child)
}
