fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    config.type_attribute(
        ".personal_agent.v1.EventEnvelope",
        "#[derive(serde::Serialize, serde::Deserialize)]",
    );
    config
        .compile_protos(
            &["../../contracts/proto/events.proto"],
            &["../../contracts/proto"],
        )
        .expect("compile protobuf contracts");
    println!("cargo:rerun-if-changed=../../contracts/proto/events.proto");
}
