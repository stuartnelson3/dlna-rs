//! `Range` header parsing — attacker-reachable on every GET to `/item/{id}`
//! (see docs/THREAT_MODEL.md). This is the exact bug class behind a real
//! MiniDLNA CVE (a chunked-length parsing overflow): checked/saturating
//! arithmetic throughout, never a raw `-`/`+` on an attacker-supplied
//! offset. Single range only — multi-range (`bytes=0-1,5-6`) is rejected,
//! not mis-implemented.

use crate::core::byte_source::ByteRange;

/// Real Range headers are one short `bytes=start-end` spec. Bound the
/// input up front, same reasoning as the SSDP/SOAP parsers.
const MAX_HEADER_LEN: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum RangeError {
    TooLarge,
    Malformed,
    MultiRange,
    /// Start beyond the file's length, start > end, or any range at all
    /// against an empty (zero-length) file.
    Unsatisfiable,
}

/// Parses a `Range` header value against a known `file_size`, producing a
/// range that's always valid to read (`0 <= start <= end < file_size`) or
/// an error describing why not. Never panics on any input.
pub fn parse(header_value: &str, file_size: u64) -> Result<ByteRange, RangeError> {
    if header_value.len() > MAX_HEADER_LEN {
        return Err(RangeError::TooLarge);
    }
    let spec = header_value
        .strip_prefix("bytes=")
        .ok_or(RangeError::Malformed)?;
    if spec.contains(',') {
        return Err(RangeError::MultiRange);
    }
    let (start_str, end_str) = spec.split_once('-').ok_or(RangeError::Malformed)?;
    let last_byte = file_size.checked_sub(1).ok_or(RangeError::Unsatisfiable)?;

    let range = match (start_str, end_str) {
        ("", "") => return Err(RangeError::Malformed),
        ("", suffix) => {
            let suffix_len: u64 = suffix.parse().map_err(|_| RangeError::Malformed)?;
            if suffix_len == 0 {
                return Err(RangeError::Unsatisfiable);
            }
            ByteRange {
                start: file_size.saturating_sub(suffix_len),
                end: last_byte,
            }
        }
        (start, "") => {
            let start: u64 = start.parse().map_err(|_| RangeError::Malformed)?;
            ByteRange {
                start,
                end: last_byte,
            }
        }
        (start, end) => {
            let start: u64 = start.parse().map_err(|_| RangeError::Malformed)?;
            let end: u64 = end.parse().map_err(|_| RangeError::Malformed)?;
            if start > end {
                return Err(RangeError::Unsatisfiable);
            }
            ByteRange {
                start,
                end: end.min(last_byte),
            }
        }
    };

    if range.start > range.end || range.start >= file_size {
        return Err(RangeError::Unsatisfiable);
    }
    Ok(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_explicit_range() {
        assert_eq!(
            parse("bytes=200-499", 1000),
            Ok(ByteRange {
                start: 200,
                end: 499
            })
        );
    }

    #[test]
    fn open_ended_range_goes_to_end_of_file() {
        assert_eq!(
            parse("bytes=200-", 1000),
            Ok(ByteRange {
                start: 200,
                end: 999
            })
        );
    }

    #[test]
    fn suffix_range_is_last_n_bytes() {
        assert_eq!(
            parse("bytes=-500", 1000),
            Ok(ByteRange {
                start: 500,
                end: 999
            })
        );
    }

    #[test]
    fn suffix_range_larger_than_file_clamps_to_whole_file() {
        assert_eq!(
            parse("bytes=-5000", 1000),
            Ok(ByteRange { start: 0, end: 999 })
        );
    }

    #[test]
    fn suffix_range_of_zero_is_unsatisfiable() {
        assert_eq!(parse("bytes=-0", 1000), Err(RangeError::Unsatisfiable));
    }

    #[test]
    fn end_beyond_file_length_clamps_to_last_byte() {
        assert_eq!(
            parse("bytes=500-999999", 1000),
            Ok(ByteRange {
                start: 500,
                end: 999
            })
        );
    }

    #[test]
    fn start_past_file_length_is_unsatisfiable() {
        assert_eq!(
            parse("bytes=1000-2000", 1000),
            Err(RangeError::Unsatisfiable)
        );
    }

    #[test]
    fn start_greater_than_end_is_unsatisfiable() {
        assert_eq!(parse("bytes=500-100", 1000), Err(RangeError::Unsatisfiable));
    }

    #[test]
    fn any_range_on_an_empty_file_is_unsatisfiable() {
        assert_eq!(parse("bytes=0-0", 0), Err(RangeError::Unsatisfiable));
        assert_eq!(parse("bytes=0-", 0), Err(RangeError::Unsatisfiable));
        assert_eq!(parse("bytes=-1", 0), Err(RangeError::Unsatisfiable));
    }

    #[test]
    fn multi_range_is_rejected() {
        assert_eq!(
            parse("bytes=0-99,200-299", 1000),
            Err(RangeError::MultiRange)
        );
    }

    #[test]
    fn missing_bytes_prefix_is_malformed() {
        assert_eq!(parse("0-499", 1000), Err(RangeError::Malformed));
    }

    #[test]
    fn no_dash_is_malformed() {
        assert_eq!(parse("bytes=500", 1000), Err(RangeError::Malformed));
    }

    #[test]
    fn dash_only_is_malformed() {
        assert_eq!(parse("bytes=-", 1000), Err(RangeError::Malformed));
    }

    #[test]
    fn non_numeric_offsets_are_malformed() {
        assert_eq!(parse("bytes=abc-def", 1000), Err(RangeError::Malformed));
    }

    #[test]
    fn oversized_header_is_rejected_before_parsing() {
        let header = format!("bytes={}", "9".repeat(MAX_HEADER_LEN));
        assert_eq!(parse(&header, 1000), Err(RangeError::TooLarge));
    }

    #[test]
    fn never_panics_on_arbitrary_short_inputs() {
        let candidates = [
            "",
            "bytes=",
            "bytes=-",
            "bytes=--",
            "bytes=18446744073709551615-18446744073709551615",
            "bytes=0-18446744073709551615",
            "BYTES=0-1",
            "bytes=0-1-2",
        ];
        for candidate in candidates {
            for file_size in [0u64, 1, u64::MAX] {
                let _ = parse(candidate, file_size);
            }
        }
    }
}
