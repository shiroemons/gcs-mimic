use std::{env, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--help"] {
        println!(
            "Usage: cargo xtask compatibility <validate|render|check> | dependencies check | release prepare TARGET BINARY OUTPUT_DIR"
        );
        return ExitCode::SUCCESS;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    if args.len() == 5 && args[..2] == ["release", "prepare"] {
        return match xtask::release::prepare(
            root,
            &args[2],
            Path::new(&args[3]),
            Path::new(&args[4]),
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }
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
        eprintln!(
            "Usage: cargo xtask compatibility <validate|render|check> | dependencies check | release prepare TARGET BINARY OUTPUT_DIR"
        );
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
