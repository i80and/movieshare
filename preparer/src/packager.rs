use anyhow::{Result, anyhow};
use mkv_element::io::blocking_impl::*;
use mkv_element::prelude::*;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct DashManifestInfo {
    // Media presentation info
    duration_ms: u64,
    timecode_scale: u64,

    // Files and their tracks
    files: Vec<FileInfo>,
}

#[derive(Debug)]
struct FileInfo {
    path: PathBuf,
    segment_position: u64,
    tracks: Vec<TrackInfo>,
    clusters: Vec<ClusterInfo>,
    cue_points: Vec<CuePoint>,
    duration: Option<f64>,
    timecode_scale: u64,
    cues_position: Option<u64>,
    cues_size: Option<u64>,
}

#[derive(Debug)]
struct TrackInfo {
    track_number: u64,
    track_uid: u64,
    track_type: TrackType,
    codec_id: String,
    codec_private: Option<Vec<u8>>,

    // Video specific
    width: Option<u64>,
    height: Option<u64>,

    // Audio specific
    sample_rate: Option<f64>,
    channels: Option<u64>,
    bit_depth: Option<u64>,

    // Common
    language: Option<String>,
    default_duration: Option<u64>, // in nanoseconds
}

#[derive(Debug, PartialEq)]
enum TrackType {
    Video,
    Audio,
    Subtitle,
    Unknown,
}

#[derive(Debug)]
struct ClusterInfo {
    timecode: u64,
    position: u64,
    size: u64,
}

#[derive(Debug)]
struct CuePoint {
    time: u64,
    track_number: u64,
    cluster_position: u64,
}

// Position tracking wrapper
struct PositionTracker<R> {
    inner: R,
    position: u64,
}

impl<R: Read> PositionTracker<R> {
    fn new(inner: R) -> Self {
        Self { inner, position: 0 }
    }

    fn position(&self) -> u64 {
        self.position
    }
}

impl<R: Read> Read for PositionTracker<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

fn collect_file_info(file_path: &Path) -> Result<FileInfo> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut tracker = PositionTracker::new(reader);

    let mut file_info = FileInfo {
        path: file_path.to_owned(),
        segment_position: 0,
        tracks: Vec::new(),
        clusters: Vec::new(),
        cue_points: Vec::new(),
        duration: None,
        timecode_scale: 1_000_000,
        cues_position: None,
        cues_size: None,
    };

    // First, read EBML header
    let _ebml = Ebml::read_from(&mut tracker)?;

    // Read segment header to get position
    let segment_start = tracker.position();
    let segment_header = Header::read_from(&mut tracker)?;

    if segment_header.id != Segment::ID {
        return Err(anyhow!("Expected Segment element"));
    }

    file_info.segment_position = segment_start;
    let segment_data_start = tracker.position();

    let segment_end = if *segment_header.size == u64::MAX {
        u64::MAX // Unknown size, goes to EOF
    } else {
        segment_data_start + *segment_header.size
    };

    while tracker.position() < segment_end {
        let element_start = tracker.position();
        let header = match Header::read_from(&mut tracker) {
            Ok(h) => h,
            Err(_) => break, // EOF or error
        };

        match header.id {
            // Info element - contains duration and timecode scale
            Info::ID => {
                let info_elem = Info::read_element(&header, &mut tracker)?;

                file_info.timecode_scale = *info_elem.timestamp_scale;

                if let Some(duration) = info_elem.duration {
                    file_info.duration = Some(*duration);
                }
            }

            // Tracks element - contains track information
            Tracks::ID => {
                let tracks_elem = Tracks::read_element(&header, &mut tracker)?;

                for track_entry in &tracks_elem.track_entry {
                    let track_type = match *track_entry.track_type {
                        1 => TrackType::Video,
                        2 => TrackType::Audio,
                        17 => TrackType::Subtitle,
                        _ => TrackType::Unknown,
                    };

                    let mut track_info = TrackInfo {
                        track_number: *track_entry.track_number,
                        track_uid: *track_entry.track_uid,
                        track_type,
                        codec_id: track_entry.codec_id.0.clone(),
                        codec_private: track_entry.codec_private.as_ref().map(|cp| cp.0.clone()),
                        width: None,
                        height: None,
                        sample_rate: None,
                        channels: None,
                        bit_depth: None,
                        language: Option::<&Language>::from(&track_entry.language)
                            .map(|l| l.0.clone()),
                        default_duration: track_entry
                            .default_duration
                            .as_ref()
                            .map(|d: &DefaultDuration| **d),
                    };

                    // Video specific
                    if let Some(video) = &track_entry.video {
                        track_info.width = Some(*video.pixel_width);
                        track_info.height = Some(*video.pixel_height);
                    }

                    // Audio specific
                    if let Some(audio) = &track_entry.audio {
                        track_info.sample_rate = Some(*audio.sampling_frequency);
                        track_info.channels = Some(*audio.channels);
                        if let Some(bd) = &audio.bit_depth {
                            track_info.bit_depth = Some(**bd);
                        }
                    }

                    file_info.tracks.push(track_info);
                }
            }

            // Cluster element - contains media data
            Cluster::ID => {
                let cluster_start = element_start;

                // Read the cluster to get timecode
                let cluster = Cluster::read_element(&header, &mut tracker)?;

                let cluster_size = tracker.position() - cluster_start;

                file_info.clusters.push(ClusterInfo {
                    timecode: *cluster.timestamp,
                    position: cluster_start,
                    size: cluster_size,
                });
            }

            // Cues element - contains seeking index
            Cues::ID => {
                let cues_start = element_start;
                let cues = Cues::read_element(&header, &mut tracker)?;
                let cues_end = tracker.position();

                // Store Cues position and size for indexRange
                file_info.cues_position = Some(cues_start);
                file_info.cues_size = Some(cues_end - cues_start);

                for cue_point in &cues.cue_point {
                    let cue_time = *cue_point.cue_time;

                    for track_pos in &cue_point.cue_track_positions {
                        file_info.cue_points.push(CuePoint {
                            time: cue_time,
                            track_number: *track_pos.cue_track,
                            cluster_position: *track_pos.cue_cluster_position,
                        });
                    }
                }
            }

            // Skip other elements we don't need for DASH
            _ => {
                // Skip the element body
                let size = *header.size;
                let mut skip_buf = vec![0u8; size.min(8192) as usize];
                let mut remaining = size;
                while remaining > 0 {
                    let to_read = remaining.min(skip_buf.len() as u64) as usize;
                    tracker.read_exact(&mut skip_buf[..to_read])?;
                    remaining -= to_read as u64;
                }
            }
        }
    }

    Ok(file_info)
}

pub fn collect_dash_info(
    file_paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<DashManifestInfo> {
    let mut info = DashManifestInfo::default();
    info.timecode_scale = 1_000_000; // default 1ms

    for file_path in file_paths {
        let file_path: &Path = file_path.as_ref();
        println!("Processing: {:?}", file_path);
        let file_info = collect_file_info(file_path)?;

        // Use the timecode scale from the file
        info.timecode_scale = file_info.timecode_scale;

        // Get duration from Info element or clusters
        let file_duration_ms = if let Some(duration) = file_info.duration {
            (duration as u64 * file_info.timecode_scale) / 1_000_000
        } else if !file_info.clusters.is_empty() {
            let last_cluster = file_info.clusters.last().unwrap();
            (last_cluster.timecode * file_info.timecode_scale) / 1_000_000
        } else {
            0
        };

        if file_duration_ms > info.duration_ms {
            info.duration_ms = file_duration_ms;
        }

        info.files.push(file_info);
    }

    Ok(info)
}

// Convert Matroska codec IDs to DASH-compliant codec strings
fn matroska_to_dash_codec(matroska_codec: &str, _track: &TrackInfo) -> String {
    match matroska_codec {
        "V_AV1" => {
            // AV1 codec string format: av01.P.LLT.DD[.M.CCC.cp.tc.mc.F]
            // For simplicity, using a common profile
            "av01.0.04M.08".to_string()
        }
        "V_VP9" => {
            // VP9 codec string format: vp09.PP.LL.DD.CC[.cp.tc.mc.F]
            "vp09.00.41.08".to_string()
        }
        "V_VP8" => "vp8".to_string(),
        "A_OPUS" => "opus".to_string(),
        "A_VORBIS" => "vorbis".to_string(),
        "A_AAC" => {
            // For AAC, ideally we'd parse CodecPrivate, but use a common default
            "mp4a.40.2".to_string()
        }
        // Fallback: return the Matroska codec ID
        _ => matroska_codec.to_string(),
    }
}

pub fn generate_dash_manifest(info: &DashManifestInfo, use_segment_list: bool) -> Result<String> {
    let duration_sec = info.duration_ms / 1000;

    let mut xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="static"
     mediaPresentationDuration="PT{}S"
     minBufferTime="PT2S"
     profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period>
"#,
        duration_sec
    );

    // Collect all video and audio tracks across all files
    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();

    for file_info in &info.files {
        for track in &file_info.tracks {
            match track.track_type {
                TrackType::Video => video_tracks.push((file_info, track)),
                TrackType::Audio => audio_tracks.push((file_info, track)),
                _ => {}
            }
        }
    }

    // Video adaptation set
    if !video_tracks.is_empty() {
        xml.push_str("    <AdaptationSet mimeType=\"video/webm\" segmentAlignment=\"true\">\n");

        for (file_info, track) in video_tracks {
            let width = track.width.unwrap_or(0);
            let height = track.height.unwrap_or(0);
            let bandwidth = estimate_bandwidth(&file_info.clusters, info.duration_ms);
            let codec = matroska_to_dash_codec(&track.codec_id, track);

            // Extract filename from path
            let filename = file_info
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(anyhow::anyhow!("Invalid filename"))?;

            xml.push_str(&format!(
                "      <Representation id=\"video_{}\" codecs=\"{}\" bandwidth=\"{}\" width=\"{}\" height=\"{}\">\n",
                track.track_number,
                codec,
                bandwidth,
                width,
                height
            ));

            xml.push_str(&format!("        <BaseURL>{}</BaseURL>\n", filename));

            if use_segment_list {
                generate_segment_list(&mut xml, file_info);
            } else {
                generate_segment_base(&mut xml, file_info);
            }

            xml.push_str("      </Representation>\n");
        }

        xml.push_str("    </AdaptationSet>\n");
    }

    // Audio adaptation set - group by language
    if !audio_tracks.is_empty() {
        // Group audio tracks by language
        let mut lang_groups: std::collections::HashMap<String, Vec<(&FileInfo, &TrackInfo)>> =
            std::collections::HashMap::new();

        for (file_info, track) in &audio_tracks {
            let lang = track.language.clone().unwrap_or_else(|| "und".to_string());
            lang_groups
                .entry(lang)
                .or_default()
                .push((file_info, track));
        }

        for (lang, tracks) in lang_groups {
            xml.push_str(&format!(
                "    <AdaptationSet mimeType=\"audio/webm\" lang=\"{}\" segmentAlignment=\"true\">\n",
                lang
            ));

            for (file_info, track) in tracks {
                let sample_rate = track.sample_rate.unwrap_or(48000.0) as u64;
                let bandwidth = estimate_bandwidth(&file_info.clusters, info.duration_ms);
                let codec = matroska_to_dash_codec(&track.codec_id, track);

                // Extract filename from path
                let filename = file_info
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or(anyhow::anyhow!("Invalid filename"))?;

                xml.push_str(&format!(
                    "      <Representation id=\"audio_{}_{}\" codecs=\"{}\" bandwidth=\"{}\" audioSamplingRate=\"{}\">\n",
                    lang,
                    track.track_number,
                    codec,
                    bandwidth,
                    sample_rate
                ));

                xml.push_str(&format!("        <BaseURL>{}</BaseURL>\n", filename));

                if use_segment_list {
                    generate_segment_list(&mut xml, file_info);
                } else {
                    generate_segment_base(&mut xml, file_info);
                }

                xml.push_str("      </Representation>\n");
            }

            xml.push_str("    </AdaptationSet>\n");
        }
    }

    xml.push_str("  </Period>\n</MPD>");
    Ok(xml)
}

// ============================================================================
// SEGMENTATION STRATEGIES
// ============================================================================
// Two approaches for VoD DASH manifests:
// 1. SegmentBase: Uses Cues element as index (smaller manifest, requires player to parse Cues)
// 2. SegmentList: Explicit byte ranges for each cluster (larger manifest, simpler for players)
//
// To switch between them, change the `use_segment_list` parameter when calling
// generate_dash_manifest(). Set to `false` to use SegmentBase (recommended if
// your files have good Cues), or `true` to use SegmentList.
// ============================================================================

fn generate_segment_base(xml: &mut String, file_info: &FileInfo) {
    if file_info.clusters.is_empty() {
        return;
    }

    let first_cluster = &file_info.clusters[0];
    let init_range_end = first_cluster.position - 1;

    // If we have a Cues element, use it as the index (most efficient)
    if let (Some(cues_pos), Some(cues_size)) = (file_info.cues_position, file_info.cues_size) {
        let cues_end = cues_pos + cues_size - 1;

        xml.push_str(&format!(
            "        <SegmentBase indexRange=\"{}-{}\">\n",
            cues_pos, cues_end
        ));
        xml.push_str(&format!(
            "          <Initialization range=\"0-{}\"/>\n",
            init_range_end
        ));
        xml.push_str("        </SegmentBase>\n");
    } else {
        // No Cues element, use entire media range
        // This is less efficient but will still work
        let last_cluster = file_info.clusters.last().unwrap();
        let index_end = last_cluster.position + last_cluster.size - 1;

        xml.push_str(&format!(
            "        <SegmentBase indexRange=\"{}-{}\">\n",
            first_cluster.position, index_end
        ));
        xml.push_str(&format!(
            "          <Initialization range=\"0-{}\"/>\n",
            init_range_end
        ));
        xml.push_str("        </SegmentBase>\n");
    }
}

fn generate_segment_list(xml: &mut String, file_info: &FileInfo) {
    if file_info.clusters.is_empty() {
        return;
    }

    let first_cluster = &file_info.clusters[0];
    let init_range_end = first_cluster.position - 1;

    xml.push_str("        <SegmentList timescale=\"1000\" duration=\"1000\">\n");
    xml.push_str(&format!(
        "          <Initialization range=\"0-{}\"/>\n",
        init_range_end
    ));

    for cluster in &file_info.clusters {
        let range_end = cluster.position + cluster.size - 1;
        xml.push_str(&format!(
            "          <SegmentURL mediaRange=\"{}-{}\"/>\n",
            cluster.position, range_end
        ));
    }

    xml.push_str("        </SegmentList>\n");
}

fn estimate_bandwidth(clusters: &[ClusterInfo], duration_ms: u64) -> u64 {
    if clusters.is_empty() || duration_ms == 0 {
        return 5_000_000; // Default 5 Mbps
    }

    let total_bytes: u64 = clusters.iter().map(|c| c.size).sum();
    let duration_sec = (duration_ms as f64) / 1000.0;

    ((total_bytes as f64 * 8.0) / duration_sec) as u64
}

// fn main() -> Result<(), Box<dyn std::error::Error>> {
//     let args: Vec<String> = env::args().skip(1).collect();

//     if args.is_empty() {
//         eprintln!(
//             "Usage: {} <file1.webm> [file2.webm] [file3.webm] ...",
//             env::args().next().unwrap()
//         );
//         eprintln!("\nExample:");
//         eprintln!(
//             "  {} video_1080p.webm audio_en0.webm audio_en1.webm audio_cn.webm",
//             env::args().next().unwrap()
//         );
//         std::process::exit(1);
//     }

//     let info = collect_dash_info(&args)?;

//     println!("\n=== Media Info ===");
//     println!(
//         "Duration: {}ms ({}s)",
//         info.duration_ms,
//         info.duration_ms / 1000
//     );
//     println!("Timecode Scale: {}", info.timecode_scale);
//     println!("Total files: {}", info.files.len());

//     for file_info in &info.files {
//         println!("\n=== File: {} ===", file_info.path);
//         println!("Segment Position: {}", file_info.segment_position);

//         println!("\nTracks:");
//         for track in &file_info.tracks {
//             println!(
//                 "  Track {} (UID: {}): {:?}",
//                 track.track_number, track.track_uid, track.track_type
//             );
//             println!("    Codec: {}", track.codec_id);
//             if let Some(w) = track.width {
//                 println!("    Resolution: {}x{}", w, track.height.unwrap_or(0));
//             }
//             if let Some(sr) = track.sample_rate {
//                 println!("    Sample Rate: {}Hz", sr);
//                 println!("    Channels: {}", track.channels.unwrap_or(0));
//             }
//             if let Some(lang) = &track.language {
//                 println!("    Language: {}", lang);
//             }
//         }

//         println!("\nClusters: {}", file_info.clusters.len());
//         for (i, cluster) in file_info.clusters.iter().take(5).enumerate() {
//             println!(
//                 "  Cluster {}: timecode={}ms, pos={}, size={} bytes",
//                 i,
//                 cluster.timecode * info.timecode_scale / 1_000_000,
//                 cluster.position,
//                 cluster.size
//             );
//         }
//         if file_info.clusters.len() > 5 {
//             println!("  ... and {} more clusters", file_info.clusters.len() - 5);
//         }

//         if let (Some(cues_pos), Some(cues_size)) = (file_info.cues_position, file_info.cues_size) {
//             println!("\nCues Element:");
//             println!("  Position: {}", cues_pos);
//             println!("  Size: {} bytes", cues_size);
//             println!("  Cue Points: {}", file_info.cue_points.len());
//         } else {
//             println!("\nNo Cues element found (will use entire media range for indexRange)");
//         }
//     }

//     let manifest = generate_dash_manifest(&info, true); // false = use SegmentBase (Cues-based)
//     println!("\n=== DASH Manifest ===\n{}", manifest);

//     // Uncomment to see SegmentList version:
//     // let manifest_list = generate_dash_manifest(&info, true);
//     // println!("\n=== DASH Manifest (SegmentList) ===\n{}", manifest_list);

//     Ok(())
// }
