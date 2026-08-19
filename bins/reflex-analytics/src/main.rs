use clap::Parser;
use reflex_analytics::AnalyticsSession;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "reflex-analytics")]
struct Opts {
    #[arg(short, long)]
    query: String,

    #[arg(long, default_value = "/tmp")]
    spill_dir: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();
    let session = AnalyticsSession::new(1024 * 1024 * 1024, opts.spill_dir);
    let result = session.execute_query(&opts.query)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
