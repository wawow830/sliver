//! The command line of sliver's programs (sliverd, sliver-agent, sliver-settings). Each declares
//! a [`Cli`]: how it's run and its options. That one declaration is what `--help` prints, what
//! [`Args`] lets the program ask for (asking for an undeclared option is a bug), what it refuses
//! (any other option), and what the man page tests hold the pages to.
//!
//! ```
//! const CLI: sliver_cli::Cli = sliver_cli::Cli {
//!     name: "demo",
//!     version: "1.0",
//!     synopsis: "demo [--socket PATH]",
//!     options: &[("--socket PATH", "where to listen"), ("-v, --verbose", "log more")],
//! };
//! let mut args = CLI.args(["--socket", "/tmp/s"].map(String::from));
//! assert_eq!(args.value("--socket").as_deref(), Some("/tmp/s"));
//! assert!(!args.flag("--verbose"));
//! assert!(args.finish().is_empty());
//! ```

/// A program's command line.
pub struct Cli {
    pub name: &'static str,
    pub version: &'static str,
    /// How it's run, one form per line.
    pub synopsis: &'static str,
    /// Each option: its spellings and value (`-v, --verbose`, `--socket PATH`), and what it does.
    /// `-h, --help` and `-V, --version` are every program's and needn't be listed.
    pub options: &'static [(&'static str, &'static str)],
}

const BUILTIN: &[(&str, &str)] = &[
    ("-h, --help", "print this and exit"),
    ("-V, --version", "print the version and exit"),
];

impl Cli {
    /// What `--help` prints.
    pub fn usage(&self) -> String {
        let all = || self.options.iter().chain(BUILTIN);
        let w = all().map(|(o, _)| o.len()).max().unwrap_or(0);
        let mut s = String::new();
        for (i, line) in self.synopsis.lines().enumerate() {
            s += &format!("{} {line}\n", if i == 0 { "usage:" } else { "      " });
        }
        s += "\noptions:\n";
        for (o, help) in all() {
            s += &format!("  {o:w$}  {help}\n");
        }
        s
    }

    /// Every spelling of every option (`-v`, `--verbose`, `--socket`), the built-ins included.
    pub fn flags(&self) -> impl Iterator<Item = &'static str> {
        let options = self.options.iter().chain(BUILTIN);
        options.flat_map(|(o, _)| o.split(", ").filter_map(|s| s.split(' ').next()))
    }

    fn declares(&self, flag: &str) -> bool {
        self.flags().any(|f| f == flag)
    }

    /// The command line, after `--help` and `--version` (which print and exit) and refusing any
    /// option this [`Cli`] doesn't declare.
    pub fn args(&'static self, argv: impl IntoIterator<Item = String>) -> Args {
        let rest: Vec<String> = argv.into_iter().collect();
        let mut args = Args { cli: self, rest };
        if args.flag("--help") || args.flag("-h") {
            print!("{}", self.usage());
            std::process::exit(0);
        }
        if args.flag("--version") || args.flag("-V") {
            println!("{} {}", self.name, self.version);
            std::process::exit(0);
        }
        if let Some(a) = args
            .rest
            .iter()
            .find(|a| a.starts_with('-') && !self.declares(a))
        {
            args.die(&format!("unknown option {a:?} (try --help)"));
        }
        args
    }
}

/// The arguments not yet taken. Each option is taken by asking for it.
pub struct Args {
    cli: &'static Cli,
    rest: Vec<String>,
}

impl Args {
    /// Takes `flag` and its value.
    pub fn value(&mut self, flag: &str) -> Option<String> {
        self.check(flag);
        let i = self.rest.iter().position(|a| a == flag)?;
        self.rest.remove(i);
        if i < self.rest.len() {
            Some(self.rest.remove(i))
        } else {
            self.die(&format!("{flag} needs a value"))
        }
    }

    /// Takes `flag`: whether it was given.
    pub fn flag(&mut self, flag: &str) -> bool {
        self.check(flag);
        let given = self.rest.iter().any(|a| a == flag);
        self.rest.retain(|a| a != flag);
        given
    }

    /// Takes `flag`'s value, parsed, or `default` without one.
    pub fn parse<T: std::str::FromStr>(&mut self, flag: &str, default: T) -> T {
        match self.value(flag) {
            None => default,
            Some(v) => v
                .parse()
                .unwrap_or_else(|_| self.die(&format!("bad value for {flag}: {v}"))),
        }
    }

    /// What's left: the operands. Exits on anything that looks like an option.
    pub fn finish(self) -> Vec<String> {
        if let Some(a) = self.rest.iter().find(|a| a.starts_with('-')) {
            self.die(&format!("{a} isn't valid here (try --help)"));
        }
        self.rest
    }

    /// Prints `msg` as this program's and exits with status 2 (bad usage).
    pub fn die(&self, msg: &str) -> ! {
        eprintln!("{}: {msg}", self.cli.name);
        std::process::exit(2)
    }

    fn check(&self, flag: &str) {
        debug_assert!(
            self.cli.declares(flag),
            "{flag} isn't in {}'s Cli",
            self.cli.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: Cli = Cli {
        name: "t",
        version: "0",
        synopsis: "t [--n N] FILE",
        options: &[("--n N", "a number"), ("-q, --quiet", "say less")],
    };

    fn args(a: &[&str]) -> Args {
        CLI.args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn options_are_taken_and_operands_remain() {
        let mut a = args(&["f", "--n", "3", "-q"]);
        assert_eq!(a.parse("--n", 0), 3);
        assert!(a.flag("-q") && !a.flag("--quiet"));
        assert_eq!(a.finish(), ["f"]);
    }

    #[test]
    fn usage_lists_every_option_and_the_flags_are_their_spellings() {
        let u = CLI.usage();
        assert!(u.starts_with("usage: t [--n N] FILE\n"));
        assert!(u.contains("  --n N          a number\n"), "{u}");
        let flags: Vec<&str> = CLI.flags().collect();
        assert_eq!(
            flags,
            ["--n", "-q", "--quiet", "-h", "--help", "-V", "--version"]
        );
    }

    #[test]
    #[should_panic(expected = "isn't in t's Cli")]
    fn asking_for_an_undeclared_option_is_a_bug() {
        args(&[]).flag("--nope");
    }
}
