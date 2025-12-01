type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    // Tell Cargo to rerun this build script if proto files change
    println!("cargo:rerun-if-changed=proto/arborter.proto");
    println!("cargo:rerun-if-changed=proto/arborter_auth.proto");
    println!("cargo:rerun-if-changed=proto/arborter_config.proto");

    build_protos()?;
    Ok(())
}

fn build_protos() -> Result<()> {
    // build arborter API
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .out_dir("proto/generated")
        .compile_protos(&["proto/arborter.proto"], &["proto"])?;

    // build arborter auth API
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .out_dir("proto/generated")
        .type_attribute(
            "xyz.aspens.arborter_auth.v1.AuthRequest",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_auth.v1.AuthResponse",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_auth.v1.InitializeAdminRequest",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_auth.v1.InitializeAdminResponse",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .compile_protos(&["proto/arborter_auth.proto"], &["proto"])?;

    // build arborter_config API
    tonic_prost_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .out_dir("proto/generated")
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Configuration",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Chain",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Market",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Token",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.TradeContract",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.GetConfigResponse",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.GetConfigRequest",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .compile_protos(&["proto/arborter_config.proto"], &["proto"])?;

    Ok(())
}
