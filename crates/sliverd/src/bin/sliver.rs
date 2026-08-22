use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str = "usage: sliver FILE";

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [argument] if argument == "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        [argument] if argument == "--version" => {
            println!("sliver {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [_path] => {
            eprintln!("sliver supervisor is unavailable");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
