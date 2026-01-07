use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use std::env;
use std::fs;
use std::path::Path;

struct EncodingBranch {
    queue1: gst::Element,
    videoscale: gst::Element,
    capsfilter: gst::Element,
    videoconvert: gst::Element,
    queue2: gst::Element,
    encoder: gst::Element,
    queue3: gst::Element,
    parser: gst::Element,
    queue4: gst::Element,
}

struct SubtitleBranch {
    queue: gst::Element,
    text_overlay: gst::Element,
    png_encoder: gst::Element,
    png_sink: gst::Element,
    webvtt_sink: gst::Element,
}

impl SubtitleBranch {
    fn new(output_dir: &str, track_id: usize) -> Result<Self> {
        // Create subtitle output directory
        let subtitle_dir = Path::new(output_dir).join(format!("subtitles_{}", track_id));
        fs::create_dir_all(&subtitle_dir)
            .context(format!("Failed to create subtitle directory: {}", subtitle_dir.display()))?;

        Ok(Self {
            queue: gst::ElementFactory::make("queue").build()?,
            text_overlay: gst::ElementFactory::make("textoverlay")
                .property("font-desc", "Sans, 24")
                .property("color", 0xFFFFFFFFu32) // White
                .property("outline-color", 0x000000FFu32) // Black outline
                .property("halignment", "center")
                .property("valignment", "bottom")
                .build()?,
            png_encoder: gst::ElementFactory::make("pngenc").build()?,
            png_sink: gst::ElementFactory::make("multifilesink")
                .property("location", subtitle_dir.join("frame_%05d.png").to_str().unwrap())
                .build()?,
            webvtt_sink: gst::ElementFactory::make("filesink")
                .property("location", subtitle_dir.join("subtitles.vtt").to_str().unwrap())
                .build()?,
        })
    }

    fn add_to_pipeline(&self, pipeline: &gst::Pipeline) -> Result<()> {
        pipeline.add_many(&[
            &self.queue,
            &self.text_overlay,
            &self.png_encoder,
            &self.png_sink,
            &self.webvtt_sink,
        ])?;
        Ok(())
    }

    fn link(&self, tee: &gst::Element) -> Result<()> {
        // Link from tee to subtitle processing
        tee.link(&self.queue)?;
        self.queue.link(&self.text_overlay)?;
        
        // Create a tee to split the stream for both PNG and WebVTT output
        let subtitle_tee = gst::ElementFactory::make("tee").build()?;
        self.text_overlay.link(&subtitle_tee)?;
        
        // PNG branch
        let png_queue = gst::ElementFactory::make("queue").build()?;
        subtitle_tee.link(&png_queue)?;
        png_queue.link(&self.png_encoder)?;
        self.png_encoder.link(&self.png_sink)?;
        
        // WebVTT branch (would need webvttenc element, but it's not commonly available)
        // For now, we'll just create a placeholder
        let webvtt_queue = gst::ElementFactory::make("queue").build()?;
        subtitle_tee.link(&webvtt_queue)?;
        // Note: In a real implementation, you would need a webvttenc element here
        // webvtt_queue.link(&self.webvtt_sink)?;
        
        // TODO: Implement proper WebVTT generation when webvttenc becomes available
        // For now, the PNG frames are generated and can be used with a separate WebVTT file
        
        Ok(())
    }
}

impl EncodingBranch {
    fn new(bitrate_mbps: u32, preset: u32, keyframe_interval: u32) -> Result<Self> {
        let bitrate_kbps = bitrate_mbps * 1000; // Convert MB/s to kbps

        // Capsfilter to limit resolution to 1080p
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", gst::IntRange::new(1, 1920))
            .field("height", gst::IntRange::new(1, 1080))
            .build();

        Ok(Self {
            queue1: gst::ElementFactory::make("queue").build()?,
            videoscale: gst::ElementFactory::make("videoscale")
                .property_from_str("method", "lanczos")
                .build()?,
            capsfilter: gst::ElementFactory::make("capsfilter")
                .property("caps", &caps)
                .build()?,
            videoconvert: gst::ElementFactory::make("videoconvert")
                .property_from_str("dither", "bayer")
                .property_from_str("chroma-mode", "full")
                .build()?,
            queue2: gst::ElementFactory::make("queue").build()?,
            encoder: gst::ElementFactory::make("svtav1enc")
                .property("preset", preset)
                .property("target-bitrate", bitrate_kbps)
                .property("intra-period-length", keyframe_interval as i32)
                .build()?,
            queue3: gst::ElementFactory::make("queue").build()?,
            parser: gst::ElementFactory::make("av1parse").build()?,
            queue4: gst::ElementFactory::make("queue").build()?,
        })
    }

    fn add_to_pipeline(&self, pipeline: &gst::Pipeline) -> Result<()> {
        pipeline.add_many(&[
            &self.queue1,
            &self.videoscale,
            &self.capsfilter,
            &self.videoconvert,
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

        // Link the encoding chain with scaling and conversion
        self.queue1.link(&self.videoscale)?;
        self.videoscale.link(&self.capsfilter)?;
        self.capsfilter.link(&self.videoconvert)?;
        self.videoconvert.link(&self.queue2)?;
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
        self.queue4.link(dashsink)?;

        Ok(())
    }
}

fn main() -> Result<()> {
    // Initialize GStreamer
    gst::init()?;

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <input-file> <output-directory>", args[0]);
        eprintln!("Example: {} test.webm ./output", args[0]);
        std::process::exit(1);
    }

    let input_file = &args[1];
    let output_dir = &args[2];

    // Ensure output directory exists
    std::fs::create_dir_all(output_dir)
        .context(format!("Failed to create output directory: {}", output_dir))?;

    // Define bitrates in MB/s
    let bitrates = vec![6, 2]; // Can easily add more: vec![8, 6, 4, 2, 1]
    let encoder_preset = 8u32;
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
        .property("location", input_file)
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
        .property("mpd-root-path", output_dir)
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
    audio_queue3.link(&dashsink)?;

    // Create and link encoding branches
    let mut branches = Vec::new();
    for bitrate in bitrates {
        let branch = EncodingBranch::new(bitrate, encoder_preset, keyframe_interval)?;
        branch.add_to_pipeline(&pipeline)?;
        branch.link(&tee, &dashsink)?;
        branches.push(branch);
    }

    // Handle dynamic pads from decodebin
    let tee_weak = tee.downgrade();
    let audio_queue1_weak = audio_queue1.downgrade();
    let output_dir_clone = output_dir.to_string();
    let pipeline_weak = pipeline.downgrade();
    
    let subtitle_track_counter = std::sync::atomic::AtomicUsize::new(0);

    decodebin.connect_pad_added(move |_dbin, src_pad| {
        let tee = match tee_weak.upgrade() {
            Some(t) => t,
            None => return,
        };

        let audio_queue1 = match audio_queue1_weak.upgrade() {
            Some(q) => q,
            None => return,
        };

        let pipeline = match pipeline_weak.upgrade() {
            Some(p) => p,
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
        } else if name.starts_with("text/") || name.starts_with("subtitle/") {
            // Handle subtitle tracks
            let track_id = subtitle_track_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            
            println!("Detected subtitle track {}, setting up processing...", track_id);
            
            // Create a new tee for subtitles
            let subtitle_tee = gst::ElementFactory::make("tee").name(&format!("subtitle_tee_{}", track_id)).build().unwrap();
            pipeline.add(&subtitle_tee).unwrap();
            
            // Link decodebin to subtitle tee
            src_pad.link(&subtitle_tee.static_pad("sink").unwrap()).unwrap();
            
            // Create subtitle branch
            let subtitle_branch = SubtitleBranch::new(&output_dir_clone, track_id).unwrap();
            subtitle_branch.add_to_pipeline(&pipeline).unwrap();
            subtitle_branch.link(&subtitle_tee).unwrap();
            
            println!("Subtitle track {} processing set up successfully", track_id);
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
