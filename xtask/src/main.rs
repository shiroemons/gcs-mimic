use std::{env, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--help"] {
        println!("Usage: cargo xtask compatibility <validate|render|check>");
        return ExitCode::SUCCESS;
    }
    if args.len() != 2 || args[0] != "compatibility" {
        eprintln!("Usage: cargo xtask compatibility <validate|render|check>");
        return ExitCode::FAILURE;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    match xtask::run(root, &args[1]) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
