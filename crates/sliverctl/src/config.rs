//! `sliverctl config …`: offline config tools (customization.md §8). They only touch files, never
//! D-Bus, because the file is the source of truth and the agent hot-reloads it. Owner: elm.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sliver_config::{
    Context, Diagnostics, Editor, Hardware, load_file, parse_cli_value, search_path,
};
use toml::Value;

pub const USAGE: &str = "\
Config (offline, edits files; comments are kept, and invalid edits are refused)
  config path                     where sliver looks for config (* = in use)
  config check [FILE]             parse + validate (exit 1 on errors)
  config get KEY                  e.g. modules.clock.format, rules[0].layout
  config set KEY VALUE            VALUE is TOML (8, true, \"x\", { key = \"F1\" }) or a bare string
  config unset KEY
  config resolve [LAYER] [CTX=VALUE...]   which layout and modules show, e.g. app_id=foot locked=true
  config import-tiny-dfr [--force]        write config.toml + style.css from tiny-dfr's config
  config import-waybar [FILE] [--force]   write config.toml from a waybar config (default ~/.config/waybar/config.jsonc|config)
  config profile [list]           profiles (* = active); `default` is config.toml
  config profile use NAME         switch (a running agent follows; `sliverctl profile NAME` asks it directly)
  config profile new NAME [--from PROFILE]   your main config plus changes, or a copy of PROFILE
  config profile rename OLD NEW | rm NAME
";

/// The file `set`/`unset`/`import` edit: the active profile's, else the user's config.toml.
fn user_config() -> PathBuf {
    sliver_config::edit_path()
}

fn active_config() -> Option<PathBuf> {
    sliver_config::find_config()
}

fn profile_cmd(rest: &[&str]) -> Result<ExitCode, String> {
    use sliver_config::profile;
    match rest {
        [] | ["list"] => {
            let active = profile::active();
            for n in profile::list() {
                let mark = if n == active { "*" } else { " " };
                let file = match profile::path(&n) {
                    Some(p) => p.display().to_string(),
                    None => "config.toml".into(),
                };
                println!("{mark} {n:<16} {file}");
            }
        }
        ["use", name] => {
            profile::set_active(name)?;
            println!("active profile: {name}");
        }
        ["new", name] => println!("{}", profile::create(name, None)?.display()),
        ["new", name, "--from", from] => {
            println!("{}", profile::create(name, Some(from))?.display())
        }
        ["rename", from, to] => profile::rename(from, to)?,
        ["rm" | "remove", name] => profile::remove(name)?,
        _ => return Err(format!("bad profile command\n\n{USAGE}")),
    }
    Ok(ExitCode::SUCCESS)
}

fn hardware() -> Hardware {
    Hardware::detect()
}

fn print_diagnostics(d: &Diagnostics) {
    for e in &d.errors {
        eprintln!("error: {e}");
    }
    for w in &d.warnings {
        eprintln!("warning: {w}");
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Table(t) => toml::to_string_pretty(t)
            .unwrap_or_default()
            .trim_end()
            .to_owned(),
        other => other.to_string(),
    }
}

pub fn run(args: &[String]) -> ExitCode {
    match run_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sliverctl config: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_inner(args: &[String]) -> Result<ExitCode, String> {
    let (cmd, rest) = args
        .split_first()
        .ok_or_else(|| format!("missing subcommand\n\n{USAGE}"))?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match (cmd.as_str(), rest.as_slice()) {
        ("help", []) => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        ("profile" | "profiles", rest) => profile_cmd(rest),
        ("path", []) => {
            let active = active_config();
            for p in search_path() {
                let mark = if Some(&p) == active.as_ref() {
                    "*"
                } else {
                    " "
                };
                println!("{mark} {}", p.display());
            }
            if let Some(p) = sliver_config::profile::active_path() {
                println!(
                    "* {} (profile {})",
                    p.display(),
                    sliver_config::profile::active()
                );
            }
            if active.is_none() {
                println!("(none found: using the built-in defaults)");
            }
            Ok(ExitCode::SUCCESS)
        }
        ("check", files) => {
            let path = match files {
                [] => active_config(),
                [f] => Some(PathBuf::from(f)),
                _ => return Err("check takes at most one FILE".into()),
            };
            let loaded = match &path {
                Some(p) => load_file(p),
                None => sliver_config::load(),
            };
            let mut d = loaded.diagnostics;
            if d.ok() {
                d.extend(loaded.config.validate(hardware()));
            }
            print_diagnostics(&d);
            let what = path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "built-in defaults".into());
            if d.ok() {
                println!("{what}: ok ({} warnings)", d.warnings.len());
                Ok(ExitCode::SUCCESS)
            } else {
                println!("{what}: {} errors", d.errors.len());
                Ok(ExitCode::FAILURE)
            }
        }
        ("get", [key]) => {
            let ed = Editor::open(&active_config().unwrap_or_else(user_config))
                .map_err(|e| e.to_string())?;
            match ed.get(key)? {
                Some(v) => {
                    println!("{}", show(&v));
                    Ok(ExitCode::SUCCESS)
                }
                None => Ok(ExitCode::FAILURE),
            }
        }
        ("set", [key, value]) => edit(|ed| ed.set(key, &parse_cli_value(value))),
        ("unset", [key]) => edit(|ed| ed.remove(key).map(|_| ())),
        ("resolve", ctx_args) => resolve(ctx_args),
        ("import-tiny-dfr", flags) => import_tiny_dfr(flags.contains(&"--force")),
        ("import-waybar", args) => import_waybar(args),
        (other, _) => Err(format!(
            "unknown or malformed config command {other:?}\n\n{USAGE}"
        )),
    }
}

fn edit(f: impl FnOnce(&mut Editor) -> Result<(), String>) -> Result<ExitCode, String> {
    let path = user_config();
    let mut ed = Editor::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    f(&mut ed)?;
    match ed.save(hardware()) {
        Ok(d) => {
            print_diagnostics(&d);
            println!("wrote {}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(d) => {
            print_diagnostics(&d);
            eprintln!("not saved: the result would be invalid");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn resolve(args: &[&str]) -> Result<ExitCode, String> {
    let (layer, ctx_args) = match args.first() {
        Some(a) if !a.contains('=') => (*a, &args[1..]),
        _ => ("base", args),
    };
    let hw = hardware();
    let mut ctx = Context::new().with("physical_esc", hw.physical_esc);
    for kv in ctx_args {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| format!("expected KEY=VALUE, got {kv:?}"))?;
        match parse_cli_value(v) {
            Value::Boolean(b) => ctx.set(k, b),
            Value::Integer(i) => ctx.set(k, i),
            Value::Float(f) => ctx.set(k, f),
            _ => ctx.set(k, v),
        };
    }
    let loaded = match active_config() {
        Some(p) => load_file(&p),
        None => sliver_config::load(),
    };
    if !loaded.diagnostics.ok() {
        print_diagnostics(&loaded.diagnostics);
        return Ok(ExitCode::FAILURE);
    }
    let c = &loaded.config;
    let r = c.resolve(layer, &ctx).map_err(|e| e.to_string())?;
    match r.rule {
        Some(i) => {
            let name = c.rules[i]
                .name
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            println!("layer {layer}: layout {:?} from rules[{i}]{name}", r.layout);
        }
        None => println!(
            "layer {layer}: layout {:?} (layer default; no rule matched)",
            r.layout
        ),
    }
    for z in &r.zones {
        let mods: Vec<String> = z
            .modules
            .iter()
            .map(|m| {
                if m.children.is_empty() {
                    m.id.to_string()
                } else {
                    format!("{}[{}]", m.id, m.children.len())
                }
            })
            .collect();
        println!("  {:<10} {}", z.name, mods.join("  "));
    }
    Ok(ExitCode::SUCCESS)
}

fn import_tiny_dfr(force: bool) -> Result<ExitCode, String> {
    let imp = sliver_config::tinydfr::import_system()?
        .ok_or("tiny-dfr isn't installed (no /usr/share/tiny-dfr/config.toml)")?;
    let cfg = user_config();
    let css = cfg.with_file_name("style.css");
    for p in [&cfg, &css] {
        if p.exists() && !force {
            return Err(format!("{} exists; use --force to overwrite", p.display()));
        }
    }
    let dir = cfg.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&cfg, &imp.config).map_err(|e| format!("{}: {e}", cfg.display()))?;
    std::fs::write(&css, &imp.stylesheet).map_err(|e| format!("{}: {e}", css.display()))?;
    for w in &imp.warnings {
        eprintln!("warning: {w}");
    }
    println!("wrote {} and {}", cfg.display(), css.display());
    Ok(ExitCode::SUCCESS)
}

fn import_waybar(args: &[&str]) -> Result<ExitCode, String> {
    let force = args.contains(&"--force");
    let files: Vec<&str> = args.iter().copied().filter(|a| *a != "--force").collect();
    let src = match files.as_slice() {
        [f] => PathBuf::from(f),
        [] => {
            let dir = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|x| !x.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .ok_or("no HOME")?
                .join("waybar");
            ["config.jsonc", "config"]
                .iter()
                .map(|n| dir.join(n))
                .find(|p| p.is_file())
                .ok_or_else(|| format!("no waybar config in {}", dir.display()))?
        }
        _ => return Err("import-waybar takes at most one FILE".into()),
    };
    // Reuse waybar's style.css when it sits next to the config (sliver accepts the GTK-isms).
    let css = src.with_file_name("style.css");
    let opts = sliver_waybar::Options {
        stylesheet: css.is_file().then(|| css.display().to_string()),
    };
    let imp = sliver_waybar::import_path(&src, &opts)?;
    let cfg = user_config();
    if cfg.exists() && !force {
        return Err(format!(
            "{} exists; use --force to overwrite",
            cfg.display()
        ));
    }
    let dir = cfg.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&cfg, &imp.toml).map_err(|e| format!("{}: {e}", cfg.display()))?;
    for n in &imp.notes {
        eprintln!(
            "{}: {}: {}",
            format!("{:?}", n.level).to_lowercase(),
            n.path,
            n.message
        );
    }
    println!(
        "wrote {} ({} modules, {} dropped)",
        cfg.display(),
        imp.modules.len(),
        imp.dropped.len()
    );
    Ok(ExitCode::SUCCESS)
}
