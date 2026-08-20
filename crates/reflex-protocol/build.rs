fn main() {
    println!("cargo:rerun-if-changed=../../proto/reflex/domain/v1/domain.proto");
    let mut config = prost_build::Config::new();
    config
        .compile_protos(
            &["../../proto/reflex/domain/v1/domain.proto"],
            &["../../proto"],
        )
        .expect("compile protobufs");
}
