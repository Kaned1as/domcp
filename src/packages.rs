pub const PACKAGES_LABEL_KEY: &str = "domcp.packages";

pub fn canonicalize(packages: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for pkg in packages {
        let trimmed = pkg.trim();
        if trimmed.is_empty() {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized.sort();
    normalized.dedup();
    normalized
}

pub fn label_value(packages: &[String]) -> String {
    canonicalize(packages).join(",")
}

pub fn parse_label_value(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }

    let raw: Vec<String> = trimmed.split(',').map(|s| s.to_string()).collect();

    Some(canonicalize(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonicalize_sorts_and_dedups() {
        let input = vec![
            " git ".to_string(),
            "openssh".to_string(),
            "git".to_string(),
            "".to_string(),
            "  ".to_string(),
        ];
        assert_eq!(
            canonicalize(&input),
            vec!["git".to_string(), "openssh".to_string()]
        );
    }

    #[test]
    fn test_label_value_serializes_canonical_list() {
        let input = vec!["openssh".to_string(), "git".to_string()];
        assert_eq!(label_value(&input), "git,openssh");
    }

    #[test]
    fn test_parse_label_value_roundtrip() {
        let value = "git,openssh";
        let parsed = parse_label_value(value).unwrap();
        assert_eq!(parsed, vec!["git".to_string(), "openssh".to_string()]);
    }

    #[test]
    fn test_parse_label_value_empty_string() {
        let parsed = parse_label_value("").unwrap();
        assert!(parsed.is_empty());
    }
}
