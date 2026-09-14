use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=NLNF_VALIDATION_CERTIFICATE_JSON");
    println!("cargo:rerun-if-env-changed=NLNF_VALIDATION_GIT_SHA");
    println!("cargo:rerun-if-env-changed=NLNF_OPENCV_RUNTIME_SHA256");
    println!("cargo:rerun-if-env-changed=NLNF_OPENCV_RUNTIME_NAME");
    let checkout_git_sha = current_git_sha().ok();
    if let Some(git_sha) = &checkout_git_sha {
        println!("cargo:rustc-env=NLNF_BUILD_GIT_SHA={git_sha}");
    }
    if let Ok(runtime_sha256) = env::var("NLNF_OPENCV_RUNTIME_SHA256") {
        let runtime_sha256 = runtime_sha256.trim();
        if !runtime_sha256.is_empty() {
            println!("cargo:rustc-env=NLNF_OPENCV_RUNTIME_SHA256={runtime_sha256}");
        }
    }
    if let Ok(runtime_name) = env::var("NLNF_OPENCV_RUNTIME_NAME") {
        let runtime_name = runtime_name.trim();
        if !runtime_name.is_empty() {
            println!("cargo:rustc-env=NLNF_OPENCV_RUNTIME_NAME={runtime_name}");
        }
    }
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
        let current_git_sha = checkout_git_sha
            .unwrap_or_else(|| panic!("auto-recall-release requires a Git checkout"));
        if let Ok(expected_git_sha) = env::var("NLNF_VALIDATION_GIT_SHA") {
            let expected_git_sha = expected_git_sha.trim();
            if expected_git_sha != current_git_sha {
                panic!(
                    "NLNF_VALIDATION_GIT_SHA does not match the current checkout: expected {current_git_sha}, got {expected_git_sha}"
                );
            }
        }
        let certificate_git_sha = serde_json::from_str::<serde_json::Value>(&certificate)
            .ok()
            .and_then(|value| {
                value
                    .get("gitSha")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| panic!("validation certificate does not contain gitSha"));
        if !certificate_git_sha.eq_ignore_ascii_case(&current_git_sha) {
            panic!(
                "validation certificate gitSha does not match the current checkout: expected {current_git_sha}, got {certificate_git_sha}"
            );
        }
        println!("cargo:rustc-env=NLNF_BUILD_GIT_SHA={current_git_sha}");
        println!("cargo:rustc-env=NLNF_VALIDATION_CERTIFICATE_JSON={certificate}");
    }
    tauri_build::build()
}

fn current_git_sha() -> Result<String, String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").map_err(|error| error.to_string())?;
    let output = Command::new("git")
        .args(["-C", &manifest_dir, "rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if sha.is_empty() {
        Err("git returned an empty revision".to_owned())
    } else {
        Ok(sha)
    }
}
