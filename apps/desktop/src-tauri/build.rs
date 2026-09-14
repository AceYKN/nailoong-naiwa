use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=NLNF_VALIDATION_CERTIFICATE_JSON");
    if env::var_os("CARGO_FEATURE_AUTO_RECALL_RELEASE").is_some() {
        let certificate = env::var("NLNF_VALIDATION_CERTIFICATE_JSON")
            .map(|value| value.lines().map(str::trim).collect::<String>())
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                panic!(
                    "auto-recall-release requires NLNF_VALIDATION_CERTIFICATE_JSON from the validation gate"
                )
            });
        println!("cargo:rustc-env=NLNF_VALIDATION_CERTIFICATE_JSON={certificate}");
    }
    tauri_build::build()
}
