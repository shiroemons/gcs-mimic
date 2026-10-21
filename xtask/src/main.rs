use std::{env, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--help"] {
        println!("Usage: cargo xtask compatibility <validate|render|check> | dependencies check");
        return ExitCode::SUCCESS;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    if args == ["dependencies", "check"] {
        return match xtask::dependencies::check(root) {
            Ok(false) => ExitCode::SUCCESS,
            Ok(true) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("CHECK_FAILED: {error}");
                ExitCode::from(2)
            }
        };
    }
    if args.len() != 2 || args[0] != "compatibility" {
        eprintln!("Usage: cargo xtask compatibility <validate|render|check>");
        return ExitCode::FAILURE;
    }
    match xtask::run(root, &args[1]) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
