use std::{env, path::Path, process::ExitCode};

const USAGE: &str = "Usage: cargo xtask compatibility <validate|render|check> | dependencies check [--scope all|release] | differential validate|run|merge|sweep [options] | lint [--target TRIPLE|--baseline] | release prepare TARGET BINARY OUTPUT_DIR | release notices OUTPUT_DIR | release container-tags VERSION [RELEASED_VERSION...]";

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--help"] {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    if args.len() >= 3 && args[..2] == ["release", "container-tags"] {
        return match xtask::container::tags(&args[2], &args[3..]) {
            Ok(tags) => {
                println!("{}", tags.join("\n"));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if args.len() == 3 && args[..2] == ["release", "notices"] {
        return match xtask::release::notices(root, Path::new(&args[2])) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }
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
    if args
        .first()
        .is_some_and(|command| command == "differential")
    {
        let result = match args.get(1).map(String::as_str) {
            Some("validate") => xtask::differential::validate(root, &args[2..]).map(|()| false),
            Some("run") => xtask::differential::run(root, &args[2..]),
            Some("merge") => xtask::differential::merge(&args[2..]).map(|()| false),
            Some("sweep") => xtask::differential::sweep(&args[2..]),
            Some("pubsub-stub") if args.len() == 2 => xtask::differential::pubsub_stub()
                .map(|()| false)
                .map_err(|e| format!("Pub/Sub stub: {e}")),
            _ => {
                eprintln!("{}", xtask::differential::USAGE);
                return ExitCode::FAILURE;
            }
        };
        return match result {
            Ok(false) => ExitCode::SUCCESS,
            Ok(true) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        };
    }
    if args.first().is_some_and(|command| command == "lint") {
        let Some(mode) = xtask::lint::mode(&args[1..]) else {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        };
        return match xtask::lint::run(root, &mode) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(2)
            }
        };
    }
    if args.len() >= 2 && args[..2] == ["dependencies", "check"] {
        let Some(scope) = xtask::dependencies::scope(&args[2..]) else {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        };
        return match xtask::dependencies::check(root, scope) {
            Ok(false) => ExitCode::SUCCESS,
            Ok(true) => ExitCode::FAILURE,
            Err(error) => {
                eprintln!("CHECK_FAILED: {error}");
                ExitCode::from(2)
            }
        };
    }
    if args.len() != 2 || args[0] != "compatibility" {
        eprintln!("{USAGE}");
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
