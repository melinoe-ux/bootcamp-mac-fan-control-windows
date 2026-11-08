// WMF Control (Windows Mac Fan Control) - by melinoe-ux (https://github.com/melinoe-ux) - MIT License
use embed_manifest::manifest::ExecutionLevel;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Port I/O and physical memory access need the driver, which needs admin rights.
        // WMF_NO_ADMIN=1 builds an as-invoker exe (useful for --demo testing).
        let level = if std::env::var_os("WMF_NO_ADMIN").is_some() {
            ExecutionLevel::AsInvoker
        } else {
            ExecutionLevel::RequireAdministrator
        };
        embed_manifest(new_manifest("WMFControl").requested_execution_level(level))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=WMF_NO_ADMIN");
}
