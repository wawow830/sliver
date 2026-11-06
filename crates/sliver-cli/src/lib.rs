//! The command line of sliver's programs (sliverd, sliver-agent, sliver-settings). Each declares
//! a [`Cli`]: how it's run, and its options, each an [`Opt`] with its spellings and whether it
//! takes a value. That one declaration is the grammar [`Cli::args`] parses with, what `--help`
//! prints, what's refused (any other option), and what the man page tests hold the pages to.
//!
//! ```
//! use sliver_cli::{Cli, Opt};
//! const CLI: Cli = Cli {
//!     name: "demo",
//!     version: "1.0",
//!     synopsis: "demo [--socket PATH] [-v] [FILE]",
//!     options: &[
//!         Opt::value(&["--socket"], "PATH", "where to listen"),
//!         Opt::flag(&["-v", "--verbose"], "log more"),
//!     ],
//! };
//! let mut args = CLI.parse(["-v", "--socket", "/tmp/s", "f"].map(String::from)).unwrap();
//! assert_eq!(args.value("--socket").as_deref(), Some("/tmp/s"));
//! assert!(args.flag("--verbose"));
//! assert_eq!(args.operands(), ["f"]);
//! ```

/// One option: its spellings, the name of its value if it takes one, and what it does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Opt {
    pub names: &'static [&'static str],
    pub value: Option<&'static str>,
    pub help: &'static str,
}

impl Opt {
    /// An option that's given or not (`-v`, `--verbose`).
    pub const fn flag(names: &'static [&'static str], help: &'static str) -> Opt {
        Opt {
            names,
            value: None,
            help,
        }
    }

    /// An option that takes a value (`--socket PATH`).
    pub const fn value(
        names: &'static [&'static str],
        value: &'static str,
        help: &'static str,
    ) -> Opt {
        Opt {
            names,
            value: Some(value),
            help,
        }
    }
}

const HELP: Opt = Opt::flag(&["-h", "--help"], "print this and exit");
const VERSION: Opt = Opt::flag(&["-V", "--version"], "print the version and exit");

/// A program's command line.
pub struct Cli {
    pub name: &'static str,
    pub version: &'static str,
    /// How it's run, one form per line.
    pub synopsis: &'static str,
    /// Its options. `-h, --help` and `-V, --version` are every program's and aren't listed.
    pub options: &'static [Opt],
}

impl Cli {
    fn all(&self) -> impl Iterator<Item = &Opt> {
        self.options.iter().chain([&HELP, &VERSION])
    }

    fn find(&self, name: &str) -> Option<&Opt> {
        self.all().find(|o| o.names.contains(&name))
    }

    /// Every spelling of every option, `-h`, `--help`, `-V` and `--version` included.
    pub fn flags(&self) -> impl Iterator<Item = &'static str> {
        self.all().flat_map(|o| o.names.iter().copied())
    }

    /// What `--help` prints.
    pub fn usage(&self) -> String {
        let spell =
            |o: &Opt| o.names.join(", ") + &o.value.map(|v| format!(" {v}")).unwrap_or_default();
        let w = self.all().map(|o| spell(o).len()).max().unwrap_or(0);
        let mut s = String::new();
        for (i, line) in self.synopsis.lines().enumerate() {
            s += &format!("{} {line}\n", if i == 0 { "usage:" } else { "      " });
        }
        s += "\noptions:\n";
        for o in self.all() {
            s += &format!("  {:w$}  {}\n", spell(o), o.help);
        }
        s
    }

    /// Parses `argv` by this declaration: each option with its value, and the operands (also
    /// everything after `--`). An error for an unknown option or a missing value.
    pub fn parse(&'static self, argv: impl IntoIterator<Item = String>) -> Result<Args, String> {
        let mut args = Args {
            options: self.options,
            given: Vec::new(),
            operands: Vec::new(),
        };
        let mut it = argv.into_iter();
        while let Some(a) = it.next() {
            if a == "--" {
                args.operands.extend(it.by_ref());
            } else if a.starts_with('-') && a.len() > 1 {
                let o = *self
                    .find(&a)
                    .ok_or_else(|| format!("unknown option {a:?}"))?;
                let value = match o.value {
                    None => None,
                    Some(v) => match it.next() {
                        Some(x) if self.find(&x).is_none() => Some(x),
                        _ => return Err(format!("{a} needs a value ({v})")),
                    },
                };
                args.given.push((o, value));
            } else {
                args.operands.push(a);
            }
        }
        Ok(args)
    }

    /// The program's arguments: [`Cli::parse`]d, with `--help` and `--version` printed and bad
    /// usage reported, each then ending the program.
    pub fn args(&'static self, argv: impl IntoIterator<Item = String>) -> Args {
        let mut args = self
            .parse(argv)
            .unwrap_or_else(|e| self.die(&format!("{e} (try --help)")));
        if args.given(&HELP).is_some() {
            print!("{}", self.usage());
            std::process::exit(0);
        }
        if args.given(&VERSION).is_some() {
            println!("{} {}", self.name, self.version);
            std::process::exit(0);
        }
        args.given.retain(|(o, _)| *o != HELP && *o != VERSION);
        args
    }

    /// Prints `msg` as this program's and exits with status 2 (bad usage).
    pub fn die(&self, msg: &str) -> ! {
        eprintln!("{}: {msg}", self.name);
        std::process::exit(2)
    }
}

/// Parsed arguments: the options given, asked for by any of their spellings, and the operands.
#[derive(Debug)]
pub struct Args {
    options: &'static [Opt],
    given: Vec<(Opt, Option<String>)>,
    operands: Vec<String>,
}

impl Args {
    fn given(&self, o: &Opt) -> Option<&Option<String>> {
        self.given
            .iter()
            .rev()
            .find(|(g, _)| g == o)
            .map(|(_, v)| v)
    }

    /// The declared option `name` spells. Asking for an undeclared one, or a flag's value, is a
    /// bug in the program, not in its command line.
    fn declared(&self, name: &str, valued: bool) -> Opt {
        let o = self.options.iter().find(|o| o.names.contains(&name));
        let o = *o.unwrap_or_else(|| panic!("{name} isn't declared in the program's Cli"));
        assert_eq!(
            o.value.is_some(),
            valued,
            "{name}: asked for as the wrong kind"
        );
        o
    }

    /// Was this option given (by any of its spellings)?
    pub fn flag(&self, name: &str) -> bool {
        self.given(&self.declared(name, false)).is_some()
    }

    /// The option's value (the last one given).
    pub fn value(&self, name: &str) -> Option<String> {
        self.given(&self.declared(name, true)).cloned().flatten()
    }

    /// The option's value parsed, `default` without one, or an error naming the option.
    pub fn parsed<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.value(name) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("bad value for {name}: {v}")),
        }
    }

    /// The operands: what isn't an option or an option's value.
    pub fn operands(&self) -> &[String] {
        &self.operands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: Cli = Cli {
        name: "t",
        version: "0",
        synopsis: "t [--n N] FILE",
        options: &[
            Opt::value(&["--n"], "N", "a number"),
            Opt::flag(&["-q", "--quiet"], "say less"),
        ],
    };

    fn parse(a: &[&str]) -> Result<Args, String> {
        CLI.parse(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn options_values_and_operands() {
        let a = parse(&["f", "--n", "3", "-q", "--", "-x"]).unwrap();
        assert_eq!(a.value("--n").as_deref(), Some("3"));
        assert!(a.flag("--quiet") && a.flag("-q"));
        assert_eq!(a.operands(), ["f", "-x"]);
    }

    #[test]
    fn the_grammar_refuses_what_it_doesnt_declare() {
        assert_eq!(parse(&["--nope"]).unwrap_err(), "unknown option \"--nope\"");
        assert_eq!(parse(&["--n"]).unwrap_err(), "--n needs a value (N)");
        // A declared option is never taken as another's value.
        assert_eq!(
            parse(&["--n", "--help"]).unwrap_err(),
            "--n needs a value (N)"
        );
    }

    #[test]
    #[should_panic(expected = "isn't declared")]
    fn asking_for_an_undeclared_option_is_a_bug() {
        parse(&[]).unwrap().flag("--nope");
    }

    #[test]
    fn usage_lists_every_option() {
        let u = CLI.usage();
        assert!(u.starts_with("usage: t [--n N] FILE\n"));
        assert!(u.contains("  --n N          a number\n"), "{u}");
        let flags: Vec<&str> = CLI.flags().collect();
        assert_eq!(
            flags,
            ["--n", "-q", "--quiet", "-h", "--help", "-V", "--version"]
        );
    }
}
