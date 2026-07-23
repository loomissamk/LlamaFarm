//! Utility functions for `LlamaFarm`.
//!
//! This module contains reusable helper functions used across the codebase.

/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
///
/// This function safely handles multi-byte UTF-8 characters (emoji, CJK, accented characters)
/// by using character boundaries instead of byte indices.
///
/// # Arguments
/// * `s` - The string to truncate
/// * `max_chars` - Maximum number of characters to keep (excluding "...")
///
/// # Returns
/// * Original string if length <= `max_chars`
/// * Truncated string with "..." appended if length > `max_chars`
///
/// # Examples
/// ```ignore
/// use llamafarm::util::truncate_with_ellipsis;
///
/// // ASCII string - no truncation needed
/// assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
///
/// // ASCII string - truncation needed
/// assert_eq!(truncate_with_ellipsis("hello world", 5), "hello...");
///
/// // Multi-byte UTF-8 (emoji) - safe truncation
/// assert_eq!(truncate_with_ellipsis("Hello 🦀 World", 8), "Hello 🦀...");
/// assert_eq!(truncate_with_ellipsis("😀😀😀😀", 2), "😀😀...");
///
/// // Empty string
/// assert_eq!(truncate_with_ellipsis("", 10), "");
/// ```
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => {
            let truncated = &s[..idx];
            // Trim trailing whitespace for cleaner output
            format!("{}...", truncated.trim_end())
        }
        None => s.to_string(),
    }
}

/// Return the greatest valid UTF-8 char boundary at or below `index`.
///
/// This mirrors `str::floor_char_boundary` behavior while remaining compatible
/// with stable toolchains where that API is not available.
pub fn floor_utf8_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }

    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Signatures that indicate a shell/script command's output shows an uncaught
/// exception or crash rather than a clean run, even when the process itself
/// returned an `Ok` exit status or the tool call otherwise "succeeded".
///
/// Broader than a plain `Traceback (most recent call last):` check: it also
/// catches compile-time Python errors (`SyntaxError:`, `IndentationError:`)
/// which never produce a traceback preamble, plus common runtime exception
/// class names and non-Python crash signatures (unhandled JS rejections, Rust
/// panics).
const OUTPUT_FAILURE_SIGNATURES: &[&str] = &[
    "Traceback (most recent call last):",
    "Unhandled Rejection",
    "panicked at",
    "SyntaxError:",
    "IndentationError:",
    "NameError:",
    "TypeError:",
    "ValueError:",
    "KeyError:",
    "AttributeError:",
    "ImportError:",
    "ModuleNotFoundError:",
    "ZeroDivisionError:",
    "UnboundLocalError:",
    "RuntimeError:",
    "IndexError:",
];

/// Check whether a command/script's captured output shows an uncaught
/// exception or crash, so callers don't have to trust a process's own exit
/// status or a preceding "it worked" claim at face value.
pub fn output_shows_uncaught_exception(output: &str) -> bool {
    OUTPUT_FAILURE_SIGNATURES
        .iter()
        .any(|sig| output.contains(sig))
}

/// Utility enum for handling optional values.
pub enum MaybeSet<T> {
    Set(T),
    Unset,
    Null,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii_no_truncation() {
        // ASCII string shorter than limit - no change
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello world", 50), "hello world");
    }

    #[test]
    fn test_truncate_ascii_with_truncation() {
        // ASCII string longer than limit - truncates
        assert_eq!(truncate_with_ellipsis("hello world", 5), "hello...");
        assert_eq!(
            truncate_with_ellipsis("This is a long message", 10),
            "This is a..."
        );
    }

    #[test]
    fn test_truncate_empty_string() {
        assert_eq!(truncate_with_ellipsis("", 10), "");
    }

    #[test]
    fn test_truncate_at_exact_boundary() {
        // String exactly at boundary - no truncation
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_emoji_single() {
        // Single emoji (4 bytes) - should not panic
        let s = "🦀";
        assert_eq!(truncate_with_ellipsis(s, 10), s);
        assert_eq!(truncate_with_ellipsis(s, 1), s);
    }

    #[test]
    fn test_truncate_emoji_multiple() {
        // Multiple emoji - safe truncation at character boundary
        let s = "😀😀😀😀"; // 4 emoji, each 4 bytes = 16 bytes total
        assert_eq!(truncate_with_ellipsis(s, 2), "😀😀...");
        assert_eq!(truncate_with_ellipsis(s, 3), "😀😀😀...");
    }

    #[test]
    fn test_truncate_mixed_ascii_emoji() {
        // Mixed ASCII and emoji
        assert_eq!(truncate_with_ellipsis("Hello 🦀 World", 8), "Hello 🦀...");
        assert_eq!(truncate_with_ellipsis("Hi 😊", 10), "Hi 😊");
    }

    #[test]
    fn test_truncate_cjk_characters() {
        // CJK characters (Chinese - each is 3 bytes)
        let s = "这是一个测试消息用来触发崩溃的中文"; // 21 characters
        let result = truncate_with_ellipsis(s, 16);
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len() - 1));
    }

    #[test]
    fn test_truncate_accented_characters() {
        // Accented characters (2 bytes each in UTF-8)
        let s = "café résumé naïve";
        assert_eq!(truncate_with_ellipsis(s, 10), "café résum...");
    }

    #[test]
    fn test_truncate_unicode_edge_case() {
        // Mix of 1-byte, 2-byte, 3-byte, and 4-byte characters
        let s = "aé你好🦀"; // 1 + 1 + 2 + 2 + 4 bytes = 10 bytes, 5 chars
        assert_eq!(truncate_with_ellipsis(s, 3), "aé你...");
    }

    #[test]
    fn test_truncate_long_string() {
        // Long ASCII string
        let s = "a".repeat(200);
        let result = truncate_with_ellipsis(&s, 50);
        assert_eq!(result.len(), 53); // 50 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_zero_max_chars() {
        // Edge case: max_chars = 0
        assert_eq!(truncate_with_ellipsis("hello", 0), "...");
    }

    #[test]
    fn test_floor_utf8_char_boundary_ascii() {
        assert_eq!(floor_utf8_char_boundary("hello", 0), 0);
        assert_eq!(floor_utf8_char_boundary("hello", 3), 3);
        assert_eq!(floor_utf8_char_boundary("hello", 99), 5);
    }

    #[test]
    fn test_floor_utf8_char_boundary_multibyte() {
        let s = "aé你🦀";
        assert_eq!(floor_utf8_char_boundary(s, 1), 1);
        // Index 2 is inside "é" (2-byte char), floor should move back to 1.
        assert_eq!(floor_utf8_char_boundary(s, 2), 1);
        // Index 5 is inside "你" (3-byte char), floor should move back to 3.
        assert_eq!(floor_utf8_char_boundary(s, 5), 3);
    }

    #[test]
    fn output_shows_uncaught_exception_detects_python_traceback() {
        let output = "Traceback (most recent call last):\n  File \"x.py\", line 1\nRuntimeError: boom";
        assert!(output_shows_uncaught_exception(output));
    }

    #[test]
    fn output_shows_uncaught_exception_detects_bare_syntax_error() {
        // Python SyntaxError/IndentationError are compile-time errors and never
        // print a "Traceback (most recent call last):" preamble.
        let output = "  File \"data_pipeline_fixed.py\", line 180\n    \"current_price\": round(current_price if current_price else (previous_close and float(previous_close) else 0), 2),\nSyntaxError: invalid syntax";
        assert!(output_shows_uncaught_exception(output));
    }

    #[test]
    fn output_shows_uncaught_exception_ignores_clean_output() {
        let output = "Fetched 42 rows\nWrote output.csv\nDone.";
        assert!(!output_shows_uncaught_exception(output));
    }

    #[test]
    fn output_shows_uncaught_exception_ignores_unrelated_mentions_of_error() {
        let output = "0 errors, 0 warnings. Build succeeded.";
        assert!(!output_shows_uncaught_exception(output));
    }
}
