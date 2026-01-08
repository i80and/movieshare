use anyhow::{Context, Result};
use clap::Parser;
use gstreamer as gst;
use gstreamer::prelude::*;

mod quality_ladder;
use quality_ladder::parse_quality_ladder;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input file path
    input_file: String,

    /// Output directory
    output_directory: String,

    /// Quality ladder in format: resolution@bitrate:resolution@bitrate (e.g., 1080@6000:480@1500)
    /// Default: 1080@6000
    #[arg(short, long, default_value = "1080@6000")]
    quality_ladder: String,

    /// Encoder to use for AV1 encoding
    /// Options: vaapi (vaav1enc) or svtav1 (svtav1enc)
    /// Default: vaapi
    #[arg(long, default_value = "vaapi")]
    encoder: String,

    /// Preset for SVT-AV1 encoder (only used when encoder is svtav1)
    /// Common presets: 0-13 (0=fastest, 13=best quality)
    #[arg(long, requires = "encoder")]
    svtav1_preset: Option<u32>,
}

/// Encoder choice for AV1 encoding
#[derive(Debug, Clone, Copy)]
enum EncoderChoice {
    Vaapi,  // Use vaav1enc (VA-API hardware acceleration)
    SvtAv1, // Use svtav1enc (software-based SVT-AV1 encoder)
}

impl std::fmt::Display for EncoderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncoderChoice::Vaapi => write!(f, "vaapi (vaav1enc)"),
            EncoderChoice::SvtAv1 => write!(f, "svtav1 (svtav1enc)"),
        }
    }
}

/// Encoder configuration
#[derive(Debug, Clone)]
struct EncoderConfig {
    choice: EncoderChoice,
    svtav1_preset: Option<u32>, // Only used for SvtAv1 encoder
}

impl EncoderConfig {
    fn from_args(encoder_str: &str, svtav1_preset: Option<u32>) -> Result<Self> {
        let choice = match encoder_str.to_lowercase().as_str() {
            "vaapi" => EncoderChoice::Vaapi,
            "svtav1" => EncoderChoice::SvtAv1,
            other => anyhow::bail!(
                "Invalid encoder choice: {}. Expected 'vaapi' or 'svtav1'",
                other
            ),
        };

        Ok(Self {
            choice,
            svtav1_preset,
        })
    }
}

struct EncodingBranch {
    queue1: gst::Element,
    vaapipostproc: gst::Element,
    capsfilter: gst::Element,
    queue2: gst::Element,
    encoder: gst::Element,
    queue3: gst::Element,
    parser: gst::Element,
    queue4: gst::Element,
}

impl EncodingBranch {
    fn new(
        resolution: u32,
        bitrate_kbps: u32,
        keyframe_interval: u32,
        encoder_config: &EncoderConfig,
    ) -> Result<Self> {
        // Treat resolution as target height and let GStreamer preserve aspect ratio
        // This allows automatic width determination based on the source aspect ratio
        let target_height = resolution as i32;

        // Capsfilter to limit height only, allowing GStreamer to preserve aspect ratio
        let caps = gst::Caps::builder("video/x-raw")
            .field("height", gst::IntRange::new(1, target_height))
            .build();

        // Create encoder based on configuration
        let encoder = match encoder_config.choice {
            EncoderChoice::Vaapi => {
                let encoder = gst::ElementFactory::make("vaav1enc")
                    .build()
                    .with_context(|| "vaav1enc encoder not available. Make sure VA-API and AV1 encoding support are installed.")?;
                encoder.set_property("bitrate", bitrate_kbps);
                encoder.set_property("key-int-max", keyframe_interval as u32);
                encoder
            }
            EncoderChoice::SvtAv1 => {
                let encoder = gst::ElementFactory::make("svtav1enc")
                    .build()
                    .with_context(|| "svtav1enc encoder not available. Make sure SVT-AV1 encoder is installed.")?;
                encoder.set_property("target-bitrate", bitrate_kbps);
                encoder.set_property("intra-period-length", keyframe_interval as i32);

                // Set preset if provided
                if let Some(preset) = encoder_config.svtav1_preset {
                    encoder.set_property("preset", preset);
                }
                encoder
            }
        };

        Ok(Self {
            queue1: gst::ElementFactory::make("queue").build()?,
            vaapipostproc: gst::ElementFactory::make("vaapipostproc").build()?,
            capsfilter: gst::ElementFactory::make("capsfilter")
                .property("caps", &caps)
                .build()?,
            queue2: gst::ElementFactory::make("queue").build()?,
            encoder,
            queue3: gst::ElementFactory::make("queue").build()?,
            parser: gst::ElementFactory::make("av1parse").build()?,
            queue4: gst::ElementFactory::make("queue").build()?,
        })
    }

    fn add_to_pipeline(&self, pipeline: &gst::Pipeline) -> Result<()> {
        pipeline.add_many(&[
            &self.queue1,
            &self.vaapipostproc,
            &self.capsfilter,
            &self.queue2,
            &self.encoder,
            &self.queue3,
            &self.parser,
            &self.queue4,
        ])?;
        Ok(())
    }

    fn link(&self, tee: &gst::Element, dashsink: &gst::Element) -> Result<()> {
        // Link from tee
        tee.link(&self.queue1)?;

        // Link the encoding chain with VA-API postprocessing
        self.queue1.link(&self.vaapipostproc)?;
        self.vaapipostproc.link(&self.capsfilter)?;
        self.capsfilter.link(&self.queue2)?;
        self.queue2.link(&self.encoder)?;
        self.encoder.link(&self.queue3)?;
        self.queue3.link(&self.parser)?;

        // Link with caps filter
        let caps = gst::Caps::builder("video/x-av1")
            .field("stream-format", "obu-stream")
            .field("alignment", "tu")
            .build();
        self.parser.link_filtered(&self.queue4, &caps)?;

        // Link to dashsink
        let video_sink_pad = dashsink
            .request_pad_simple("video_%u")
            .context("Failed to get video pad from dashsink")?;
        let video_src_pad = self
            .queue4
            .static_pad("src")
            .context("Failed to get src pad from queue4")?;
        video_src_pad.link(&video_sink_pad)?;

        Ok(())
    }
}

fn main() -> Result<()> {
    // Initialize GStreamer
    gst::init()?;

    // Parse command line arguments using clap
    let args = Args::parse();

    let input_file = args.input_file;
    let output_dir = args.output_directory;

    // Ensure output directory exists
    std::fs::create_dir_all(&output_dir)
        .context(format!("Failed to create output directory: {}", output_dir))?;

    // Parse quality ladder
    let quality_ladder = parse_quality_ladder(&args.quality_ladder)?;
    println!("Using quality ladder: {:?}", quality_ladder);

    // Parse encoder configuration
    let encoder_config = EncoderConfig::from_args(&args.encoder, args.svtav1_preset)?;
    println!("Using encoder: {}", encoder_config.choice);
    if let Some(preset) = encoder_config.svtav1_preset {
        println!("Using SVT-AV1 preset: {}", preset);
    }

    let target_duration = 4u32; // seconds

    // Calculate keyframe interval (assuming 30fps, adjust if needed)
    // For variable framerate, this will be approximate
    let fps = 30u32;
    let keyframe_interval = fps * target_duration; // 120 frames for 4 seconds at 30fps

    // Create the pipeline
    let pipeline = gst::Pipeline::new();

    // Create source and decoder elements
    let filesrc = gst::ElementFactory::make("filesrc")
        .name("filesrc")
        .property("location", &input_file)
        .build()?;

    let decodebin = gst::ElementFactory::make("decodebin").name("d").build()?;

    let tee = gst::ElementFactory::make("tee").name("t").build()?;

    // Audio processing elements
    let audio_queue1 = gst::ElementFactory::make("queue").build()?;
    let audioconvert = gst::ElementFactory::make("audioconvert").build()?;
    let audioresample = gst::ElementFactory::make("audioresample").build()?;
    let audio_queue2 = gst::ElementFactory::make("queue").build()?;

    let opusenc = gst::ElementFactory::make("opusenc")
        .property("bitrate", 192000i32)
        .build()?;

    let audio_queue3 = gst::ElementFactory::make("queue").build()?;

    // DASH sink with output directory
    let dashsink = gst::ElementFactory::make("dashsink")
        .property("mpd-filename", "manifest.mpd")
        .property("mpd-root-path", &output_dir)
        .property("target-duration", target_duration)
        .property_from_str("muxer", "dashmp4")
        .build()?;

    // Add base elements to pipeline
    pipeline.add_many(&[
        &filesrc,
        &decodebin,
        &tee,
        &audio_queue1,
        &audioconvert,
        &audioresample,
        &audio_queue2,
        &opusenc,
        &audio_queue3,
        &dashsink,
    ])?;

    // Link static elements
    filesrc.link(&decodebin)?;

    // Link audio processing chain
    audio_queue1.link(&audioconvert)?;
    audioconvert.link(&audioresample)?;
    audioresample.link(&audio_queue2)?;

    // Link audio with caps filter to ensure stereo
    let audio_caps = gst::Caps::builder("audio/x-raw")
        .field("channels", 2i32)
        .build();
    audio_queue2.link_filtered(&opusenc, &audio_caps)?;
    opusenc.link(&audio_queue3)?;

    let audio_sink_pad = dashsink
        .request_pad_simple("audio_%u")
        .context("Failed to get audio pad from dashsink")?;
    let audio_src_pad = audio_queue3
        .static_pad("src")
        .context("Failed to get src pad from audio_queue3")?;
    audio_src_pad.link(&audio_sink_pad)?;

    // Create and link encoding branches
    let mut branches = Vec::new();
    for (resolution, bitrate_kbps) in quality_ladder {
        let branch =
            EncodingBranch::new(resolution, bitrate_kbps, keyframe_interval, &encoder_config)?;
        branch.add_to_pipeline(&pipeline)?;
        branch.link(&tee, &dashsink)?;
        branches.push(branch);
    }

    // Handle dynamic pads from decodebin
    let tee_weak = tee.downgrade();
    let audio_queue1_weak = audio_queue1.downgrade();

    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let tee = match tee_weak.upgrade() {
            Some(t) => t,
            None => return,
        };

        let audio_queue1 = match audio_queue1_weak.upgrade() {
            Some(q) => q,
            None => return,
        };

        // Get pad caps
        let caps = src_pad.current_caps().unwrap();
        let structure = caps.structure(0).unwrap();
        let name = structure.name();

        if name.starts_with("video/") {
            let sink_pad = tee.static_pad("sink").unwrap();
            if !sink_pad.is_linked() {
                src_pad
                    .link(&sink_pad)
                    .expect("Failed to link decodebin video to tee");
            }
        } else if name.starts_with("audio/") {
            let sink_pad = audio_queue1.static_pad("sink").unwrap();
            if !sink_pad.is_linked() {
                src_pad
                    .link(&sink_pad)
                    .expect("Failed to link decodebin audio to queue");
            }
        }
    });

    // Start playing
    println!("Starting transcoding...");
    println!("Input: {}", input_file);
    println!("Output: {}", output_dir);

    pipeline.set_state(gst::State::Playing)?;

    // Wait until error or EOS
    let bus = pipeline.bus().unwrap();
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;

        match msg.view() {
            MessageView::Eos(..) => {
                println!("Transcoding complete!");
                break;
            }
            MessageView::Error(err) => {
                eprintln!(
                    "Error from {:?}: {} ({:?})",
                    err.src().map(|s| s.path_string()),
                    err.error(),
                    err.debug()
                );
                break;
            }
            MessageView::StateChanged(state) => {
                if msg.src().map(|s| s == &pipeline).unwrap_or(false) {
                    if state.current() == gst::State::Playing {
                        println!("Pipeline is now playing...");
                    }
                }
            }
            _ => (),
        }
    }

    // Clean up
    pipeline.set_state(gst::State::Null)?;

    Ok(())
}
