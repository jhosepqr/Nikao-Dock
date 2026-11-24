fn main() {
    // `option_env!("MUELLE_FORK_BUILD")` is resolved while this crate compiles.
    // Cargo does not notice an env var on its own, so a cached official build
    // would otherwise be reused for a fork installer.
    println!("cargo:rerun-if-env-changed=MUELLE_FORK_BUILD");
}
