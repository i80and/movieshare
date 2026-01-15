use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_end_to_end_transcoding() -> Result<()> {
    // 1. Setup test environment
    let test_dir = tempdir().context("Failed to create temp directory")?;
    let test_path = test_dir.path();

    use preparer::run_transcoding;

    run_transcoding(
        Some(&PathBuf::from("tests/data/big-buck-bunny.toml")),
        None,
        &test_path,
        "1080@6000:480@1500",
        "svtav1",
        Some(13),
    )
    .context("Failed to run transcoding")?;

    // 3. Validate output files exist and have reasonable sizes
    let expected_files = [
        "video_1080p.mp4",
        "video_480p.mp4",
        "audio_en.mp4",
        "manifest.mpd",
    ];

    for file in expected_files {
        let file_path = test_path.join(file);
        assert!(file_path.exists(), "Expected file {} not found", file);

        let metadata =
            fs::metadata(&file_path).context(format!("Failed to get metadata for {}", file))?;
        let file_size = metadata.len();
        assert!(file_size > 0, "File {} is empty", file);
    }

    // 4. Validate keyframe intervals using ffprobe
    validate_keyframe_intervals(&test_path.join("video_1080p.mp4"))?;
    validate_keyframe_intervals(&test_path.join("video_480p.mp4"))?;

    // Test passes - temp_dir will be automatically cleaned up
    Ok(())
}

fn validate_keyframe_intervals(video_path: &Path) -> Result<()> {
    // Run ffprobe to get keyframe timestamps
    let output = Command::new("ffprobe")
        .args([
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=key_frame,best_effort_timestamp_time",
            "-output_format",
            "csv",
            video_path.to_str().unwrap(),
        ])
        .output()
        .context("Failed to run ffprobe")?;

    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse ffprobe output
    let stdout = String::from_utf8(output.stdout)?;
    let mut keyframe_times = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() >= 3 && parts[1] == "1" {
            // key_frame=1 (second column)
            if let Ok(time) = parts[2].parse::<f64>() {
                keyframe_times.push(time);
            }
        }
    }

    // Check that we have keyframes
    assert!(!keyframe_times.is_empty(), "No keyframes found in video");

    // Validate 4-second intervals with tolerance
    let tolerance = 0.5; // ±0.5 seconds tolerance
    let expected_interval = 4.0;

    for i in 1..keyframe_times.len() {
        let actual_interval = keyframe_times[i] - keyframe_times[i - 1];
        let diff = (actual_interval - expected_interval).abs();

        assert!(
            diff <= tolerance,
            "Keyframe interval {} is not within tolerance of expected {} (diff: {})",
            actual_interval,
            expected_interval,
            diff
        );
    }

    Ok(())
}
