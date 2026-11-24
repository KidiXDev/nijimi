use std::{env, error::Error, path::Path, process::Command};

use wasm_bindgen_cli_support::Bindgen;

const WASM_TARGET: &str = "wasm32-unknown-unknown";

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask must live inside the workspace")?;
    let target_dir = root.join("target");

    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(root)
        .args(["build", "--release", "--package", "nijimi-wasm"])
        .args(["--target", WASM_TARGET, "--target-dir"])
        .arg(&target_dir)
        .status()?;
    if !status.success() {
        return Err("the WASM build failed".into());
    }

    Bindgen::new()
        .input_path(
            target_dir
                .join(WASM_TARGET)
                .join("release/nijimi_wasm.wasm"),
        )
        .out_name("nijimi")
        .web(true)?
        .omit_default_module_path(false)
        .typescript(true)
        .remove_name_section(true)
        .generate(root.join("pkg"))?;
    Ok(())
}
