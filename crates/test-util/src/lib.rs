pub mod fs;

pub use cealn_test_util_macro::fs_test;

use std::{env, fmt::Write, sync::Once};

use tracing_subscriber::fmt::format::FmtSpan;

static INIT: Once = Once::new();

pub fn prep() {
    INIT.call_once(|| {
        let mut filter = env::var("CEALN_LOG").unwrap_or_else(|_| "debug".to_owned());

        // Some dependencies have INSANELY verbose logging (cranelift is particularly awful, it writes EVERY instruction
        // it emits at debug level) so we force them off
        write!(&mut filter, ",cranelift_codegen=warn,cranelift_wasm=warn,regalloc=warn").unwrap();

        let filter = tracing_subscriber::EnvFilter::try_new(filter)
            // Error out on invalid RUST_LOG value instead of ignoring to make it easier to diagnose logging mistakes
            // (they're just tests so suicide is an option)
            .unwrap();

        tracing_subscriber::fmt::fmt()
            .with_span_events(FmtSpan::FULL)
            .with_file(true)
            .with_line_number(true)
            .with_target(false)
            .with_env_filter(filter)
            .init();
    });
}
