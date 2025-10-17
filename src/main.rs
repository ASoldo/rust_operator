mod controller;
mod crd;
mod resources;

use crate::{controller::run_operator, crd::print_crd_without_formats};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    if std::env::var("PRINT_CRD").is_ok() {
        print_crd_without_formats()?;
        return Ok(());
    }

    run_operator().await
}
