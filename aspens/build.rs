type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Shared pure transform — also unit-tested under
// `aspens/tests/build_attestation_paths.rs`.
mod build_attestation_paths;

fn main() -> Result<()> {
    // Tell Cargo to rerun this build script if proto files change
    println!("cargo:rerun-if-changed=proto/arborter.proto");
    println!("cargo:rerun-if-changed=proto/arborter_auth.proto");
    println!("cargo:rerun-if-changed=proto/arborter_config.proto");
    println!("cargo:rerun-if-changed=proto/attestation.proto");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    println!("cargo:rerun-if-env-changed=ASPENS_REGEN_PROTOS");

    // The generated bindings under proto/generated/ are committed and ship with
    // the crate, so by default we DO NOT regenerate them. tonic_prost_build's
    // out_dir below is the source tree (`proto/generated`), not `OUT_DIR`, so
    // regenerating on every build dirties the tree — which breaks `cargo publish`
    // verification ("source directory was modified by build.rs"), produces
    // spurious diffs, and would force every downstream consumer (and docs.rs) to
    // have `protoc`. Shipping the committed files lets them build with none.
    //
    // After editing a *.proto, regenerate explicitly and commit the result:
    //   ASPENS_REGEN_PROTOS=1 cargo build -p aspens
    let force = std::env::var_os("ASPENS_REGEN_PROTOS").is_some();
    if (!force && generated_present()) || std::env::var_os("DOCS_RS").is_some() {
        return Ok(());
    }

    build_protos()?;
    Ok(())
}

/// True when every generated binding is already present under
/// `proto/generated/` (the committed, shipped set). Codegen is skipped in that
/// case unless `ASPENS_REGEN_PROTOS` forces it.
fn generated_present() -> bool {
    use std::path::Path;
    const GENERATED: [&str; 4] = [
        "xyz.aspens.arborter.v1.rs",
        "xyz.aspens.arborter_auth.v1.rs",
        "xyz.aspens.arborter_config.v1.rs",
        "xyz.aspens.attestation.v1.rs",
    ];
    GENERATED
        .iter()
        .all(|f| Path::new("proto/generated").join(f).exists())
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
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Chain",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Market",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.Token",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.TradeContract",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.GetConfigResponse",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .type_attribute(
            "xyz.aspens.arborter_config.v1.GetConfigRequest",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"camelCase\")]",
        )
        .compile_protos(&["proto/arborter_config.proto"], &["proto"])?;

    // Post-process the generated arborter_config file to fix attestation type references.
    // The generated code uses relative `super::super::super::attestation::v1::` paths,
    // but we need absolute `crate::attestation::v1::` paths for proper module resolution.
    fix_attestation_paths()?;

    Ok(())
}

fn fix_attestation_paths() -> Result<()> {
    use std::fs;

    let config_file = "proto/generated/xyz.aspens.arborter_config.v1.rs";
    let content = fs::read_to_string(config_file)?;
    let fixed_content = build_attestation_paths::rewrite_attestation_paths(&content);
    fs::write(config_file, fixed_content)?;
    Ok(())
}
