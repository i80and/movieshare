use anyhow::Result;
use clap::Parser;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// TOML configuration file for input streams
    #[arg(long)]
    config: Option<String>,

    /// Input file path (required if no config file)
    #[arg(long = "input-file")]
    input_file: Option<String>,

    /// Output directory
    output_directory: String,

    /// Quality ladder in format: resolution@bitrate:resolution@bitrate (e.g., 1080@6000:480@1500)
    /// Default: 1080@6000
    #[arg(short, long, default_value = "1080@6000")]
    quality_ladder: String,

    /// Encoder to use for AV1 encoding
    /// Options: vaapi (vaav1enc) or svtav1 (svtav1enc)
    /// Default: vaapi
    #[arg(long, default_value = "svtav1")]
    encoder: String,

    /// Preset for SVT-AV1 encoder (only used when encoder is svtav1)
    /// Common presets: 0-13 (0=best quality, 13=fastest)
    #[arg(long, requires = "encoder")]
    svtav1_preset: Option<u32>,
}

fn main() -> Result<()> {
    // Initialize tracing with environment filter
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Parse command line arguments using clap
    let args = Args::parse();

    // Call the core transcoding function
    preparer::run_transcoding(
        args.config.as_deref(),
        args.input_file.as_deref(),
        &args.output_directory,
        &args.quality_ladder,
        &args.encoder,
        args.svtav1_preset,
    )
}
