//! Hand-rolled parser for the Test262 YAML frontmatter. Test262 frontmatter
//! is a small YAML subset: `key: value` lines at zero indent (values may be
//! `[a, b]` lists or `|` block scalars), with `negative:` carrying a
//! two-space-indented `phase:` / `type:` pair.

#[derive(Debug, Default)]
pub struct Meta {
    pub flags: Vec<String>,
    pub includes: Vec<String>,
    pub negative_phase: Option<String>,
    pub negative_type: Option<String>,
}

/// Extracts and parses the `/*--- ... ---*/` block, if present.
pub fn parse(source: &str) -> Result<Meta, String> {
    let mut meta = Meta::default();
    let Some(start) = source.find("/*---") else {
        return Ok(meta); // no frontmatter: implicit-harness defaults only
    };
    let after = &source[start + 5..];
    let Some(end) = after.find("---*/") else {
        return Err("unterminated frontmatter".to_string());
    };
    let body = &after[..end];

    let mut in_negative = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let Some((key, value)) = trimmed.split_once(':') else {
            // Continuation of a block scalar we do not care about.
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        if indent > 0 {
            // Nested under the previous key (only `negative` matters).
            if in_negative {
                match key {
                    "phase" => meta.negative_phase = Some(value.to_string()),
                    "type" => meta.negative_type = Some(value.to_string()),
                    _ => {}
                }
            }
            continue;
        }

        in_negative = key == "negative";
        match key {
            "flags" => meta.flags = parse_list(value),
            "includes" => meta.includes = parse_list(value),
            "negative" if !value.is_empty() && value != "|" && value != ">" => {
                // Inline negative (rare): not supported by the runner.
                return Err("inline negative mapping".to_string());
            }
            "negative" => {}
            _ => {
                // info/description/esid/... — skipped, including their block
                // scalar continuations (handled by the indent check above).
            }
        }
    }
    Ok(meta)
}

/// Parses `[a, b, c]` (or a bare scalar) into a list.
fn parse_list(value: &str) -> Vec<String> {
    let inner = value
        .trim()
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|item| {
            item.trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_string()
        })
        .filter(|item| !item.is_empty())
        .collect()
}
