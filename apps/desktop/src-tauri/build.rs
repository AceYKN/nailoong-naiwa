use std::{env, fs, path::PathBuf, process::Command};

use sha2::{Digest, Sha256};

const EXPECTED_OPENCV_RUNTIME_NAME: &str = "opencv_world4130";

fn main() {
    println!("cargo:rerun-if-env-changed=NLNF_VALIDATION_CERTIFICATE_JSON");
    println!("cargo:rerun-if-env-changed=NLNF_VALIDATION_GIT_SHA");
    println!("cargo:rerun-if-env-changed=NLNF_OPENCV_RUNTIME_SHA256");
    println!("cargo:rerun-if-env-changed=NLNF_OPENCV_RUNTIME_NAME");
    println!("cargo:rerun-if-env-changed=OPENCV_DIR");
    println!("cargo:rerun-if-env-changed=OPENCV_RUNTIME_DIR");
    println!("cargo:rerun-if-env-changed=OPENCV_WORLD_NAME");
    let checkout_git_sha = current_git_sha().ok();
    if let Some(git_sha) = &checkout_git_sha {
        println!("cargo:rustc-env=NLNF_BUILD_GIT_SHA={git_sha}");
    }

    let release_feature_enabled = env::var_os("CARGO_FEATURE_AUTO_RECALL_RELEASE").is_some();
    let runtime_identity =
        if env::var_os("CARGO_FEATURE_OPENCV_BACKEND").is_some() && target_is_windows() {
            match read_opencv_runtime_identity() {
                Ok(identity) => {
                    println!("cargo:rerun-if-changed={}", identity.path.display());
                    println!("cargo:rustc-env=NLNF_OPENCV_RUNTIME_NAME={}", identity.name);
                    println!(
                        "cargo:rustc-env=NLNF_OPENCV_RUNTIME_SHA256={}",
                        identity.sha256
                    );
                    Some(identity)
                }
                Err(error) if release_feature_enabled => {
                    panic!("auto-recall-release requires the exact OpenCV runtime: {error}");
                }
                Err(error) => {
                    println!("cargo:warning=OpenCV runtime identity was not embedded: {error}");
                    None
                }
            }
        } else {
            None
        };

    if release_feature_enabled {
        let runtime_identity = runtime_identity.as_ref().unwrap_or_else(|| {
            panic!("auto-recall-release requires a Windows OpenCV backend and an exact runtime DLL")
        });
        ensure_clean_checkout().unwrap_or_else(|error| {
            panic!("auto-recall-release requires a clean Git checkout: {error}")
        });
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
        let certificate_value = serde_json::from_str::<serde_json::Value>(&certificate)
            .unwrap_or_else(|error| panic!("validation certificate is not valid JSON: {error}"));
        let certificate_runtime_name = certificate_value
            .get("nativeRuntimeName")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| panic!("validation certificate does not contain nativeRuntimeName"));
        let certificate_runtime_sha256 = certificate_value
            .get("nativeRuntimeSha256")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                panic!("validation certificate does not contain nativeRuntimeSha256")
            });
        if !certificate_runtime_name.eq_ignore_ascii_case(&runtime_identity.name) {
            panic!(
                "validation certificate nativeRuntimeName does not match the actual runtime: expected {}, got {}",
                runtime_identity.name, certificate_runtime_name
            );
        }
        if !certificate_runtime_sha256.eq_ignore_ascii_case(&runtime_identity.sha256) {
            panic!(
                "validation certificate nativeRuntimeSha256 does not match the actual runtime: expected {}, got {}",
                runtime_identity.sha256, certificate_runtime_sha256
            );
        }
        println!("cargo:rustc-env=NLNF_BUILD_GIT_SHA={current_git_sha}");
        println!("cargo:rustc-env=NLNF_VALIDATION_CERTIFICATE_JSON={certificate}");
    }
    tauri_build::build()
}

struct OpencvRuntimeIdentity {
    name: String,
    sha256: String,
    path: PathBuf,
}

fn read_opencv_runtime_identity() -> Result<OpencvRuntimeIdentity, String> {
    let runtime_name =
        env::var("OPENCV_WORLD_NAME").unwrap_or_else(|_| EXPECTED_OPENCV_RUNTIME_NAME.to_owned());
    if runtime_name != EXPECTED_OPENCV_RUNTIME_NAME {
        return Err(format!(
            "OPENCV_WORLD_NAME must be {EXPECTED_OPENCV_RUNTIME_NAME}, got {runtime_name}"
        ));
    }
    let runtime_directory = env::var_os("OPENCV_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("OPENCV_DIR").map(|directory| {
                PathBuf::from(directory)
                    .join("x64")
                    .join("vc16")
                    .join("bin")
            })
        })
        .ok_or_else(|| "set OPENCV_RUNTIME_DIR or OPENCV_DIR".to_owned())?;
    let runtime_path = runtime_directory.join(format!("{runtime_name}.dll"));
    if !runtime_path.is_file() {
        return Err(format!(
            "runtime DLL was not found: {}",
            runtime_path.display()
        ));
    }
    let bytes = fs::read(&runtime_path).map_err(|error| {
        format!(
            "unable to read runtime DLL {}: {error}",
            runtime_path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sha256 = hex::encode(hasher.finalize());

    validate_optional_runtime_environment(&runtime_name, &sha256)?;
    Ok(OpencvRuntimeIdentity {
        name: runtime_name,
        sha256,
        path: runtime_path,
    })
}

fn validate_optional_runtime_environment(
    runtime_name: &str,
    runtime_sha256: &str,
) -> Result<(), String> {
    if let Ok(expected_name) = env::var("NLNF_OPENCV_RUNTIME_NAME") {
        let expected_name = expected_name.trim();
        if !expected_name.is_empty() && !expected_name.eq_ignore_ascii_case(runtime_name) {
            return Err(format!(
                "NLNF_OPENCV_RUNTIME_NAME does not match the actual runtime: expected {runtime_name}, got {expected_name}"
            ));
        }
    }
    if let Ok(expected_sha256) = env::var("NLNF_OPENCV_RUNTIME_SHA256") {
        let expected_sha256 = expected_sha256.trim();
        if !expected_sha256.is_empty() && !expected_sha256.eq_ignore_ascii_case(runtime_sha256) {
            return Err(format!(
                "NLNF_OPENCV_RUNTIME_SHA256 does not match the actual runtime: expected {runtime_sha256}, got {expected_sha256}"
            ));
        }
    }
    Ok(())
}

fn target_is_windows() -> bool {
    env::var("CARGO_CFG_TARGET_OS")
        .map(|target_os| target_os == "windows")
        .unwrap_or(false)
}

fn ensure_clean_checkout() -> Result<(), String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").map_err(|error| error.to_string())?;
    let output = Command::new("git")
        .args([
            "-C",
            &manifest_dir,
            "status",
            "--porcelain",
            "--untracked-files=all",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let status = String::from_utf8_lossy(&output.stdout);
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(format!(
            "working tree has uncommitted changes: {}",
            status.trim()
        ))
    }
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
