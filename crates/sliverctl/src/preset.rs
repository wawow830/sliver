//! `sliverctl preset …`: offline `.sliver` bundles (ux.md §9, `sliver_config::preset`). Like
//! `config`, this only touches files; the agent hot-reloads what `import` writes.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sliver_config::preset::{Bundle, CONFIG, EXTENSION, Run, export};

pub const USAGE: &str = "\
Presets (offline: .sliver files hold a config, its includes, stylesheet and icons)
  preset export FILE [--name NAME] [--config PATH]   bundle your config (env tables are left out)
  preset show FILE                                   files, everything it runs, and whether it loads
  preset import FILE [--yes] [--no-commands] [--dir DIR]
                                  replace your config with it; replaced files go to backups/
";

pub fn run(args: &[String]) -> ExitCode {
    match run_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sliverctl preset: {e}");
            ExitCode::from(2)
        }
    }
}

struct Opts {
    file: String,
    name: Option<String>,
    config: Option<PathBuf>,
    dir: Option<PathBuf>,
    yes: bool,
    no_commands: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        file: String::new(),
        name: None,
        config: None,
        dir: None,
        yes: false,
        no_commands: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |what: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs {what}"))
        };
        match a.as_str() {
            "--name" => o.name = Some(val("a NAME")?),
            "--config" => o.config = Some(val("a PATH")?.into()),
            "--dir" => o.dir = Some(val("a DIR")?.into()),
            "--yes" | "-y" => o.yes = true,
            "--no-commands" => o.no_commands = true,
            s if s.starts_with('-') && s != "-" => return Err(format!("unknown option {s}")),
            s if o.file.is_empty() => o.file = s.to_owned(),
            s => return Err(format!("unexpected {s:?}")),
        }
    }
    if o.file.is_empty() {
        return Err("missing FILE".into());
    }
    Ok(o)
}

fn run_inner(args: &[String]) -> Result<ExitCode, String> {
    let (cmd, rest) = args
        .split_first()
        .ok_or_else(|| format!("missing subcommand\n\n{USAGE}"))?;
    if matches!(cmd.as_str(), "help" | "-h" | "--help") {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let o = parse(rest)?;
    match cmd.as_str() {
        "export" => do_export(&o),
        "show" => do_show(&o),
        "import" => do_import(&o),
        other => Err(format!("unknown subcommand {other:?}\n\n{USAGE}")),
    }
}

/// Where the user's config lives: `$XDG_CONFIG_HOME/sliver` or `~/.config/sliver`.
fn user_dir() -> Result<PathBuf, String> {
    sliver_config::search_path()
        .into_iter()
        .next()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| "no HOME or XDG_CONFIG_HOME".into())
}

fn read(file: &str) -> Result<Bundle, String> {
    let text = if file == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| e.to_string())?;
        s
    } else {
        std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?
    };
    Bundle::parse(&text).map_err(|e| format!("{file}: {e}"))
}

fn do_export(o: &Opts) -> Result<ExitCode, String> {
    let config = match &o.config {
        Some(p) => p.clone(),
        None => sliver_config::find_config()
            .ok_or("nothing to export: you're on the built-in defaults (no config.toml)")?,
    };
    let ex = export(&config, o.name.as_deref())?;
    for n in &ex.notes {
        eprintln!("note: {n}");
    }
    let text = ex.bundle.to_text();
    if o.file == "-" {
        print!("{text}");
    } else {
        let mut file = PathBuf::from(&o.file);
        if file.extension().is_none() {
            file.set_extension(EXTENSION);
        }
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
        eprintln!(
            "wrote {} ({} files from {})",
            file.display(),
            ex.bundle.files.len(),
            config.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn print_summary(b: &Bundle, runs: &[Run]) {
    if let Some(n) = &b.name {
        println!("preset: {n}");
    }
    println!("files:");
    for (n, d) in &b.files {
        println!("  {n}  ({} bytes)", d.len());
    }
    if runs.is_empty() {
        println!("runs: nothing (no scripts, commands or D-Bus calls)");
    } else {
        println!("runs, as you:");
        for r in runs {
            println!("  {r}");
        }
    }
}

fn scratch() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|x| !x.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from)
}

fn do_show(o: &Opts) -> Result<ExitCode, String> {
    let b = read(&o.file)?;
    print_summary(&b, &b.runs());
    let d = b.check(&scratch())?;
    for w in &d.warnings {
        println!("warning: {w}");
    }
    for e in &d.errors {
        println!("error: {e}");
    }
    if d.ok() {
        println!("loads: yes");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("loads: no (import would refuse it)");
        Ok(ExitCode::FAILURE)
    }
}

/// What the user chose at the review prompt.
#[derive(Debug, PartialEq)]
enum Choice {
    All,
    NoCommands,
    Cancel,
}

fn ask(question: &str, runs: bool) -> Result<Choice, String> {
    if !std::io::stdin().is_terminal() {
        return Err(if runs {
            "review the preset (`sliverctl preset show FILE`), then pass --yes, or --no-commands \
             to import it with its commands removed"
                .into()
        } else {
            "pass --yes to replace your config (not a terminal, so I can't ask)".into()
        });
    }
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(match line.trim() {
        "yes" | "y" if !runs => Choice::All,
        "yes" if runs => Choice::All,
        "no-commands" if runs => Choice::NoCommands,
        _ => Choice::Cancel,
    })
}

fn do_import(o: &Opts) -> Result<ExitCode, String> {
    let b = read(&o.file)?;
    let dir = match &o.dir {
        Some(d) => d.clone(),
        None => user_dir()?,
    };
    let runs = b.runs();
    let choice = if o.no_commands {
        Choice::NoCommands
    } else if o.yes {
        Choice::All
    } else {
        print_summary(&b, &runs);
        let q = if runs.is_empty() {
            format!(
                "Replace your config in {}? (a backup is kept) [y/N] ",
                dir.display()
            )
        } else {
            format!(
                "It runs {} command(s) as you. Type 'yes' to import it as is, 'no-commands' to \
                 import it without them, or anything else to cancel: ",
                runs.len()
            )
        };
        ask(&q, !runs.is_empty())?
    };
    let b = match choice {
        Choice::Cancel => {
            eprintln!("cancelled; nothing changed");
            return Ok(ExitCode::FAILURE);
        }
        Choice::All => b,
        Choice::NoCommands => {
            let (clean, n) = b.without_commands();
            if n > 0 {
                eprintln!("removed {n} command(s) and script(s)");
            }
            clean
        }
    };
    let inst = b.install(&dir)?;
    if let Some(bk) = &inst.backup {
        eprintln!("backed up the files it replaced to {}", bk.display());
    }
    eprintln!(
        "imported {} file(s) into {}; the agent reloads {CONFIG} by itself",
        inst.written.len(),
        dir.display()
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn options() {
        let o = parse(&s(&["a.sliver", "--yes", "--dir", "/x", "--name", "N"])).unwrap();
        assert_eq!(o.file, "a.sliver");
        assert!(o.yes && !o.no_commands);
        assert_eq!(o.dir, Some(PathBuf::from("/x")));
        assert_eq!(o.name.as_deref(), Some("N"));
        assert!(parse(&s(&[])).is_err());
        assert!(parse(&s(&["a", "b"])).is_err());
        assert!(parse(&s(&["a", "--bogus"])).is_err());
        assert!(parse(&s(&["a", "--dir"])).is_err());
        assert_eq!(parse(&s(&["-"])).unwrap().file, "-");
    }
}
