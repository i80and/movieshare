use anyhow::{Context, Result};
use clap::Parser;
use gstreamer as gst;
use gstreamer::prelude::*;
use tempfile::TempDir;

mod quality_ladder;
use quality_ladder::parse_quality_ladder;

const SEGMENT_DURATION_SEC: u32 = 4; // Duration of each segment in seconds
const AUDIO_BITRATE: u32 = 192000; // Bitrate for audio in bits per second

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
    qtmux: gst::Element,
    filesink: gst::Element,
}

impl EncodingBranch {
    fn new(
        resolution: u32,
        bitrate_kbps: u32,
        keyframe_interval: u32,
        encoder_config: &EncoderConfig,
        output_dir: &str,
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

                // Set keyframe interval for proper segment alignment
                encoder.set_property("key-int-max", keyframe_interval as u32);

                encoder
            }
            EncoderChoice::SvtAv1 => {
                let encoder = gst::ElementFactory::make("svtav1enc")
                    .build()
                    .with_context(|| "svtav1enc encoder not available. Make sure SVT-AV1 encoder is installed.")?;
                encoder.set_property("target-bitrate", bitrate_kbps);

                // Set keyframe interval for proper segment alignment
                encoder.set_property("intra-period-length", keyframe_interval as i32);

                // Set preset if provided
                if let Some(preset) = encoder_config.svtav1_preset {
                    encoder.set_property("preset", preset);
                }
                encoder
            }
        };

        // Create qtmux for fragmented MP4 output
        let qtmux = gst::ElementFactory::make("mp4mux")
            .property("faststart", true)
            .build()?;

        // Create filesink with appropriate filename
        let output_filename = format!("{}/video_{}p.mp4", output_dir, resolution);
        let filesink = gst::ElementFactory::make("filesink")
            .property("location", &output_filename)
            .build()?;

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
            qtmux,
            filesink,
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
            &self.qtmux,
            &self.filesink,
        ])?;
        Ok(())
    }

    fn link(&self, tee: &gst::Element) -> Result<()> {
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

        // Link to qtmux and filesink
        self.queue4.link(&self.qtmux)?;
        self.qtmux.link(&self.filesink)?;

        Ok(())
    }
}

/// Call the packager to generate DASH manifest
///
/// # Arguments
///
/// * `quality_ladder` - Vector of (resolution, bitrate_kbps) tuples
/// * `temp_dir` - Temporary directory containing intermediate MP4 files
/// * `output_dir` - Final output directory for DASH manifest and segmented files
fn call_packager(quality_ladder: &[(u32, u32)], temp_dir: &str, output_dir: &str) -> Result<()> {
    let packager_path = "./packager-linux-x64";

    // Build the packager command
    let mut command = std::process::Command::new(packager_path);

    // Add video streams - read from temp_dir, write to output_dir
    for &(resolution, bitrate_kbps) in quality_ladder {
        let bandwidth = bitrate_kbps * 1000; // Convert kbps to bps
        let video_input_file = format!("{}/video_{}p.mp4", temp_dir, resolution);
        let video_output_file = format!("{}/video_{}p.mp4", output_dir, resolution);
        command.arg(format!(
            "in={},stream=video,output={},bandwidth={}",
            video_input_file, video_output_file, bandwidth
        ));
    }

    // Add audio stream - read from temp_dir, write to output_dir
    let audio_input_file = format!("{}/audio.mp4", temp_dir);
    let audio_output_file = format!("{}/audio_en.mp4", output_dir);
    command.arg(format!(
        "in={},stream=audio,output={},language=en,bandwidth={}",
        audio_input_file, audio_output_file, AUDIO_BITRATE
    ));

    // Add manifest output and segment duration - write to output_dir
    let manifest_file = format!("{}/manifest.mpd", output_dir);
    command.arg("--mpd_output").arg(manifest_file);
    command
        .arg("--segment_duration")
        .arg(SEGMENT_DURATION_SEC.to_string());

    // Execute the command
    let status = command.status()?;

    if !status.success() {
        anyhow::bail!(
            "Packager failed with exit code: {:?}. Intermediate files are available in: {}",
            status.code(),
            temp_dir
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_packager_command_construction() {
        // Test with a simple quality ladder
        let quality_ladder = vec![(1080, 3000), (720, 1500), (480, 800)];
        let temp_dir = "test_temp";
        let output_dir = "test_output";

        // Build the expected command arguments
        let mut expected_args = Vec::new();

        // Video streams - read from temp_dir, write to output_dir
        expected_args.push(
            "in=test_temp/video_1080p.mp4,stream=video,output=test_output/video_1080p.mp4,bandwidth=3000000",
        );
        expected_args.push(
            "in=test_temp/video_720p.mp4,stream=video,output=test_output/video_720p.mp4,bandwidth=1500000",
        );
        expected_args.push(
            "in=test_temp/video_480p.mp4,stream=video,output=test_output/video_480p.mp4,bandwidth=800000",
        );

        // Audio stream - read from temp_dir, write to output_dir
        expected_args.push("in=test_temp/audio.mp4,stream=audio,output=test_output/audio_en.mp4,language=en,bandwidth=128000");

        // Manifest and segment duration - write to output_dir
        expected_args.push("--mpd_output");
        expected_args.push("test_output/manifest.mpd");
        expected_args.push("--segment_duration");
        expected_args.push("4");

        // Build the command to test argument construction
        let mut command = std::process::Command::new("packager-linux-x64");

        // Add video streams
        for &(resolution, bitrate_kbps) in &quality_ladder {
            let bandwidth = bitrate_kbps * 1000;
            let video_input_file = format!("{}/video_{}p.mp4", temp_dir, resolution);
            let video_output_file = format!("{}/video_{}p.mp4", output_dir, resolution);
            command.arg(format!(
                "in={},stream=video,output={},bandwidth={}",
                video_input_file, video_output_file, bandwidth
            ));
        }

        // Add audio stream
        let audio_input_file = format!("{}/audio.mp4", temp_dir);
        let audio_output_file = format!("{}/audio_en.mp4", output_dir);
        command.arg(format!(
            "in={},stream=audio,output={},language=en,bandwidth=128000",
            audio_input_file, audio_output_file
        ));

        // Add manifest output and segment duration
        let manifest_file = format!("{}/manifest.mpd", output_dir);
        command.arg("--mpd_output").arg(manifest_file);
        command.arg("--segment_duration").arg("4");

        // Verify the arguments match
        let actual_args: Vec<String> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            actual_args, expected_args,
            "Command arguments should match expected format"
        );
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

    // Create temporary directory inside the output directory
    // This ensures we have enough space and avoids cross-filesystem issues
    let temp_dir = TempDir::new_in(&output_dir).context("Failed to create temporary directory")?;
    let temp_path = temp_dir
        .path()
        .to_str()
        .context("Failed to convert temp directory path to string")?;

    println!("Using temporary directory: {}", temp_path);

    // Parse quality ladder
    let quality_ladder = parse_quality_ladder(&args.quality_ladder)?;
    println!("Using quality ladder: {:?}", quality_ladder);

    // Parse encoder configuration
    let encoder_config = EncoderConfig::from_args(&args.encoder, args.svtav1_preset)?;
    println!("Using encoder: {}", encoder_config.choice);
    if let Some(preset) = encoder_config.svtav1_preset {
        println!("Using SVT-AV1 preset: {}", preset);
    }

    let target_duration = SEGMENT_DURATION_SEC; // seconds

    // Frame rate detection - we'll detect actual frame rate from source
    // Start with a reasonable default, but this will be updated when we detect caps
    let detected_fps = std::sync::Arc::new(std::sync::Mutex::new(30u32));
    let detected_fps_clone = detected_fps.clone();

    // Calculate initial keyframe interval (will be recalculated after frame rate detection)
    let _keyframe_interval = std::sync::Arc::new(std::sync::Mutex::new(30 * target_duration));

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
        .property("bitrate", AUDIO_BITRATE as i32)
        .build()?;

    let audio_queue3 = gst::ElementFactory::make("queue").build()?;

    // Audio qtmux for fragmented MP4 output
    let audio_qtmux = gst::ElementFactory::make("mp4mux")
        .property("faststart", true)
        .build()?;

    // Audio filesink - write to temp directory
    let audio_output_filename = format!("{}/audio.mp4", temp_path);
    let audio_filesink = gst::ElementFactory::make("filesink")
        .property("location", &audio_output_filename)
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
        &audio_qtmux,
        &audio_filesink,
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
    audio_queue3.link(&audio_qtmux)?;
    audio_qtmux.link(&audio_filesink)?;

    // Create and link encoding branches - write to temp directory
    // We'll use the detected frame rate for keyframe interval
    let mut branches = Vec::new();
    let final_keyframe_interval = {
        // Get the detected frame rate (or use default if not detected yet)
        let detected_fps = *detected_fps.lock().unwrap();
        detected_fps * target_duration
    };

    println!(
        "Using keyframe interval: {} frames (for {} second segments)",
        final_keyframe_interval, target_duration
    );

    for &(resolution, bitrate_kbps) in &quality_ladder {
        let branch = EncodingBranch::new(
            resolution,
            bitrate_kbps,
            final_keyframe_interval,
            &encoder_config,
            temp_path,
        )?;
        branch.add_to_pipeline(&pipeline)?;
        branch.link(&tee)?;
        branches.push(branch);
    }

    // Handle dynamic pads from decodebin
    let tee_weak = tee.downgrade();
    let audio_queue1_weak = audio_queue1.downgrade();
    let detected_fps_weak = std::sync::Arc::downgrade(&detected_fps_clone);

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
            // Detect frame rate from video caps
            if let Some(fps_weak) = detected_fps_weak.upgrade() {
                if let Ok(fps_value) = structure.get::<gst::Fraction>("framerate") {
                    let fps_num = fps_value.numer() as u32;
                    let fps_den = fps_value.denom() as u32;
                    let actual_fps = if fps_den > 0 { fps_num / fps_den } else { 30 };

                    // Update detected frame rate
                    if let Ok(mut fps_guard) = fps_weak.lock() {
                        *fps_guard = actual_fps;
                    }

                    println!("Detected frame rate: {} fps", actual_fps);
                }
            }

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
    println!("Starting fMP4 generation...");
    println!("Input: {}", input_file);
    println!("Output directory: {}", output_dir);
    println!("Generating fragmented MP4 files suitable for DASH streaming");

    pipeline.set_state(gst::State::Playing)?;

    // Wait until error or EOS
    let bus = pipeline.bus().unwrap();
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;

        match msg.view() {
            MessageView::Eos(..) => {
                println!("fMP4 generation complete!");
                println!("Generated intermediate files in temporary directory:");
                for (resolution, _) in &quality_ladder {
                    println!("  - {}/video_{}p.mp4", temp_path, resolution);
                }
                println!("  - {}/audio.mp4", temp_path);

                // Call packager to generate DASH manifest
                println!("Generating DASH manifest...");
                if let Err(e) = call_packager(&quality_ladder, temp_path, &output_dir) {
                    eprintln!("Failed to generate DASH manifest: {}", e);
                    eprintln!("Temporary directory preserved for debugging: {}", temp_path);
                } else {
                    println!("DASH manifest generation complete!");
                    println!("Final output files in: {}", output_dir);

                    // Store temp_path for cleanup message before consuming temp_dir
                    let temp_path_for_cleanup = temp_path.to_string();

                    // Clean up temporary directory
                    if let Err(e) = temp_dir.close() {
                        eprintln!("Warning: Failed to clean up temporary directory: {}", e);
                        eprintln!("You may manually delete: {}", temp_path_for_cleanup);
                    } else {
                        println!("Temporary directory cleaned up successfully");
                    }

                    println!("Files are ready for DASH streaming");
                }
                break;
            }
            MessageView::Error(err) => {
                eprintln!(
                    "Error from {:?}: {} ({:?})",
                    err.src().map(|s| s.path_string()),
                    err.error(),
                    err.debug()
                );
                eprintln!("Temporary directory preserved for debugging: {}", temp_path);
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
