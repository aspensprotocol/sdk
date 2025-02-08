fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_protos()?;
    Ok(())
}

fn build_protos() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .out_dir("proto/generated")
        .compile_protos(
            &["proto/arborter.proto", "proto/arborter_config.proto"],
            &["proto"],
        )?;

    Ok(())
}
