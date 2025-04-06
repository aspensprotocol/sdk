type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    build_protos()?;
    Ok(())
}

fn build_protos() -> Result<()> {
    // build arborter API
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .out_dir("proto/generated")
        .compile_protos(&["proto/arborter.proto"], &["proto"])?;

    // build arborter_config API
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .build_server(false)
        .build_client(true)
        .out_dir("proto/generated")
        .type_attribute(
            "xyz.aspens.arborter_config.Configuration",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.ConfigRequest",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.TradeContract",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.Chain",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.Market",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.DeployContractRequest",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.DeployContractReply",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.AddMarketReply",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.Token",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.AddTokenRequest",
            "#[derive(serde::Serialize, serde::Deserialize)]",
        )
        .compile_protos(&["proto/arborter_config.proto"], &["proto"])?;

    Ok(())
}
