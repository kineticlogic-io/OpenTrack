//! Compiles the peat-node sidecar proto with protox (pure Rust), so building
//! OpenTrack does not need `protoc` installed.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../../proto/peat_sidecar.proto";
    println!("cargo:rerun-if-changed={proto}");
    let fds = protox::compile([proto], ["../../proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .compile_fds(fds)?;
    Ok(())
}
