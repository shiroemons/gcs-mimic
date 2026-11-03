use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

fn output_dir(root: &Path, mut args: impl Iterator<Item = OsString>) -> io::Result<PathBuf> {
    let Some(flag) = args.next() else {
        return Ok(root.join("generated"));
    };
    if flag != "--out-dir" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: grpc-codegen [--out-dir <directory>]",
        ));
    }
    let path = args.next().filter(|path| !path.is_empty()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--out-dir requires a directory",
        )
    })?;
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected argument after --out-dir",
        ));
    }
    Ok(path.into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing grpc directory")?;
    let output = output_dir(root, std::env::args_os().skip(1))?;
    std::fs::create_dir_all(&output)?;
    // Object data uses bytes::Bytes so decoded payloads and read chunks pass to and
    // from the body streams without an extra copy into or out of a Vec.
    // Generate legacy first so shared packages retain the v2 foundation output.
    tonic_prost_build::configure()
        .build_client(false)
        .bytes(".google.storage.v1.ChecksummedData.content")
        .generate_default_stubs(true)
        .extern_path(".google.api", "crate::grpc::generated::google::api")
        .extern_path(".google.iam.v1", "crate::grpc::generated::google::iam::v1")
        .out_dir(&output)
        .compile_protos(
            &[root.join("proto/google/storage/v1/storage.proto")],
            &[root.join("proto")],
        )?;
    tonic_prost_build::configure()
        .build_client(false)
        .bytes(".google.storage.v2.ChecksummedData.content")
        .out_dir(&output)
        .include_file("mod.rs")
        .compile_protos(
            &[
                root.join("proto/google/storage/v2/storage.proto"),
                root.join("proto/google/rpc/error_details.proto"),
            ],
            &[root.join("proto")],
        )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_directory_defaults_and_rejects_invalid_arguments() {
        let root = Path::new("grpc");
        assert_eq!(
            output_dir(root, [].into_iter()).unwrap(),
            root.join("generated")
        );
        assert_eq!(
            output_dir(
                root,
                ["--out-dir", "output"].map(OsString::from).into_iter()
            )
            .unwrap(),
            Path::new("output")
        );
        for args in [
            vec!["--unknown"],
            vec!["--out-dir"],
            vec!["--out-dir", ""],
            vec!["--out-dir", "output", "extra"],
        ] {
            assert_eq!(
                output_dir(root, args.into_iter().map(OsString::from))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
