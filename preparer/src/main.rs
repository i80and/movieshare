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
    fn new(resolution: u32, bitrate_kbps: u32, keyframe_interval: u32) -> Result<Self> {
        // Calculate max width and height based on resolution
        let (max_width, max_height) = match resolution {
            2160 => (3840, 2160), // 4K
            1440 => (2560, 1440), // 2K
            1080 => (1920, 1080), // 1080p
            720 => (1280, 720),   // 720p
            480 => (854, 480),    // 480p
            360 => (640, 360),    // 360p
            240 => (426, 240),    // 240p
            _ => (1920, 1080),    // Default to 1080p if unknown
        };

        // Capsfilter to limit resolution
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", gst::IntRange::new(1, max_width))
            .field("height", gst::IntRange::new(1, max_height))
            .build();

        Ok(Self {
            queue1: gst::ElementFactory::make("queue").build()?,
            vaapipostproc: gst::ElementFactory::make("vaapipostproc").build()?,
            capsfilter: gst::ElementFactory::make("capsfilter")
                .property("caps", &caps)
                .build()?,
            queue2: gst::ElementFactory::make("queue").build()?,
            encoder: gst::ElementFactory::make("vaav1enc")
                .property("bitrate", bitrate_kbps)
                .property("key-int-max", keyframe_interval as u32)
                .build()?,
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
        let branch = EncodingBranch::new(resolution, bitrate_kbps, keyframe_interval)?;
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
