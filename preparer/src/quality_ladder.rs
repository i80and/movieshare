use anyhow::{Context, Result};

/// Parse quality ladder string into resolution and bitrate pairs
///
/// # Arguments
///
/// * `quality_ladder` - String in format "resolution@bitrate:resolution@bitrate"
///
/// # Returns
///
/// Vector of tuples (resolution, bitrate_kbps)
///
/// # Examples
///
/// ```
/// let ladder = parse_quality_ladder("1080@6000:480@1500")?;
/// assert_eq!(ladder, vec![(1080, 6000), (480, 1500)]);
/// ```

pub fn parse_quality_ladder(quality_ladder: &str) -> Result<Vec<(u32, u32)>> {
    let mut ladder = Vec::new();
    
    for part in quality_ladder.split(':') {
        let part = part.trim();
        if part.is_empty() {
            continue; // Skip empty parts
        }
        
        let parts: Vec<&str> = part.split('@').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid quality ladder format. Expected resolution@bitrate, got: {}", part);
        }
        
        let resolution = parts[0].trim().parse::<u32>()
            .context(format!("Failed to parse resolution from: {}", parts[0]))?;
        
        let bitrate_kbps = parts[1].trim().parse::<u32>()
            .context(format!("Failed to parse bitrate from: {}", parts[1]))?;
            
        ladder.push((resolution, bitrate_kbps));
    }
    
    if ladder.is_empty() {
        anyhow::bail!("Quality ladder cannot be empty");
    }
    
    Ok(ladder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_quality_ladder() {
        let result = parse_quality_ladder("1080@6000:480@1500").unwrap();
        assert_eq!(result, vec![(1080, 6000), (480, 1500)]);
    }

    #[test]
    fn test_parse_single_quality() {
        let result = parse_quality_ladder("720@2000").unwrap();
        assert_eq!(result, vec![(720, 2000)]);
    }

    #[test]
    fn test_parse_multiple_qualities() {
        let result = parse_quality_ladder("2160@12000:1080@8000:720@4000:480@2000").unwrap();
        assert_eq!(result, vec![
            (2160, 12000),
            (1080, 8000),
            (720, 4000),
            (480, 2000)
        ]);
    }

    #[test]
    fn test_parse_invalid_format_missing_at() {
        let result = parse_quality_ladder("10806000");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Invalid quality ladder format"));
    }

    #[test]
    fn test_parse_invalid_resolution() {
        let result = parse_quality_ladder("abc@1500");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to parse resolution"));
    }

    #[test]
    fn test_parse_invalid_bitrate() {
        let result = parse_quality_ladder("1080@xyz");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to parse bitrate"));
    }

    #[test]
    fn test_parse_empty_bitrate() {
        let result = parse_quality_ladder("1080@");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to parse bitrate"));
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_quality_ladder("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Quality ladder cannot be empty"));
    }

    #[test]
    fn test_parse_whitespace() {
        let result = parse_quality_ladder("  1080@6000  :  480@1500  ").unwrap();
        assert_eq!(result, vec![(1080, 6000), (480, 1500)]);
    }

    #[test]
    fn test_parse_zero_values() {
        let result = parse_quality_ladder("0@0").unwrap();
        assert_eq!(result, vec![(0, 0)]);
    }

    #[test]
    fn test_parse_large_values() {
        let result = parse_quality_ladder("4320@50000:2160@25000").unwrap();
        assert_eq!(result, vec![(4320, 50000), (2160, 25000)]);
    }
}