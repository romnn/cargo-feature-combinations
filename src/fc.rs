use anyhow::Result;

fn main() -> Result<()> {
    let bin_name = env!("CARGO_BIN_NAME");
    let bin_name = bin_name.strip_prefix("cargo-").unwrap_or(bin_name);
    cargo_feature_combinations::run(bin_name)
}
