use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing grpc directory")?;
    // Generate legacy first so shared packages retain the v2 foundation output.
    tonic_prost_build::configure()
        .build_client(false)
        .generate_default_stubs(true)
        .extern_path(".google.api", "crate::grpc::generated::google::api")
        .extern_path(".google.iam.v1", "crate::grpc::generated::google::iam::v1")
        .out_dir(root.join("generated"))
        .compile_protos(
            &[root.join("proto/google/storage/v1/storage.proto")],
            &[root.join("proto")],
        )?;
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
