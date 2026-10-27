use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing grpc directory")?;
    tonic_prost_build::configure()
        .build_client(false)
        .out_dir(root.join("generated"))
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
