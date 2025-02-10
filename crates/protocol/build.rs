fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure().compile(
        &["proto/event.proto", "proto/file.proto", "proto/workspace_builder.proto"],
        &["proto"],
    )?;
    Ok(())
}
