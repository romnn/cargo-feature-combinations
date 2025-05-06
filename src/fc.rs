use color_eyre::eyre;

#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> eyre::Result<()> {
    let bin_name = env!("CARGO_BIN_NAME");
    let bin_name = bin_name.strip_prefix("cargo-").unwrap_or(bin_name);
    cargo_feature_combinations::run(bin_name)
}
