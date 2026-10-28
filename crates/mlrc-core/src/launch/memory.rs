//! Memory values in the `-Xmx4G` sense.
//!
//! Java accepts `512m`, `2G`, `1024M`. Users write `4g` and `4096`; both are
//! accepted here and normalised so a config file, a flag and a log line all agree.

use crate::error::{ErrorCode, MlrcError, MlrcResult};

/// Returns the value with a lowercase unit, or a usage error naming the problem.
pub fn normalize(value: &str) -> MlrcResult<String> {
    let text = value.trim();
    if text.is_empty() {
        return Err(invalid(value, "expected a size such as 4G or 2048M"));
    }

    let (digits, unit) = match text.char_indices().find(|(_, ch)| !ch.is_ascii_digit()) {
        Some((index, _)) => (&text[..index], &text[index..]),
        None => (text, ""),
    };

    if digits.is_empty() {
        return Err(invalid(value, "expected a number first, such as 4G"));
    }
    let amount: u64 = digits
        .parse()
        .map_err(|_| invalid(value, "that number is too large"))?;
    if amount == 0 {
        return Err(invalid(value, "memory must be greater than zero"));
    }

    // No unit means megabytes, matching how people talk about heaps ("2048").
    let unit = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "m" | "mb" => "M",
        "g" | "gb" => "G",
        "t" | "tb" => "T",
        "k" | "kb" => "K",
        other => {
            return Err(invalid(
                value,
                &format!("unknown unit {other}, expected K, M, G or T"),
            ))
        }
    };

    Ok(format!("{amount}{unit}"))
}

fn invalid(value: &str, reason: &str) -> MlrcError {
    MlrcError::new(
        ErrorCode::Usage,
        format!("invalid memory value {value}: {reason}"),
    )
    .with("value", value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_are_normalised() {
        assert_eq!(normalize("4g").unwrap(), "4G");
        assert_eq!(normalize(" 2048M ").unwrap(), "2048M");
        assert_eq!(normalize("1T").unwrap(), "1T");
        assert_eq!(normalize("512").unwrap(), "512M");
        assert_eq!(normalize("4096mb").unwrap(), "4096M");
    }

    #[test]
    fn nonsense_is_rejected() {
        assert!(normalize("").is_err());
        assert!(normalize("G").is_err());
        assert!(normalize("0").is_err());
        assert!(normalize("4x").is_err());
        assert!(normalize("-4G").is_err());
    }
}
