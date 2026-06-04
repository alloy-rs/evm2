/// Optional test-name filter.
#[derive(Clone, Debug, Default)]
pub struct EntryPoint {
    pattern: Option<String>,
}

impl EntryPoint {
    /// Creates a filter from an optional glob pattern.
    #[inline]
    pub const fn new(pattern: Option<String>) -> Self {
        Self { pattern }
    }

    /// Returns whether `name` is selected by this filter.
    #[inline]
    pub fn matches(&self, name: &str) -> bool {
        self.pattern.as_ref().is_none_or(|pattern| wildcard_match(pattern, name))
    }
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }

    let mut remaining = value;
    if let Some(first) = parts.first().copied()
        && !first.is_empty()
    {
        let Some(stripped) = remaining.strip_prefix(first) else {
            return false;
        };
        remaining = stripped;
    }

    for part in parts.iter().skip(1).take(parts.len().saturating_sub(2)) {
        if part.is_empty() {
            continue;
        }
        let Some(index) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[index + part.len()..];
    }

    if let Some(last) = parts.last().copied()
        && !last.is_empty()
    {
        return remaining.ends_with(last);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::wildcard_match;

    #[test]
    fn wildcard_match_supports_prefix_middle_and_suffix() {
        assert!(wildcard_match("foo*bar*baz", "foo-hello-bar-world-baz"));
        assert!(wildcard_match("*bar", "foo-bar"));
        assert!(wildcard_match("foo*", "foo-bar"));
        assert!(!wildcard_match("foo*baz", "foo-bar"));
    }
}
