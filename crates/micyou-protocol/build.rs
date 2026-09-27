/* libmicyou — headless, frontend-decoupled backend for MicYou. */

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Point prost-build at the vendored protoc binary so the build works
    // without a system protobuf installation (same approach as upstream).
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    prost_build::compile_protos(&["proto/network.proto"], &["proto/"])?;
    Ok(())
}
