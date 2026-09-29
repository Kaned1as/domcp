use crate::dockerfile::Runner;

pub fn build_parts(runner: Runner, command: &[String]) -> Vec<String> {
    match runner {
        Runner::Uvx => {
            // uvx runs as: uvx <package> [args...]
            command.to_vec()
        }
        Runner::Pipx => {
            // pipx run <package> [args...]
            let mut v = vec!["pipx".to_string(), "run".to_string()];
            v.extend(command.iter().skip(1).cloned());
            v
        }
        Runner::Npx => {
            // npx [flags] <package> [args...]
            command.to_vec()
        }
    }
}

/// Build the ENTRYPOINT instruction for the Dockerfile.
pub fn build(runner: Runner, command: &[String]) -> String {
    let parts = build_parts(runner, command);
    let json = serde_json::to_string(&parts).unwrap_or_else(|_| "[]".to_string());
    format!("ENTRYPOINT {}", json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_parts_uvx() {
        let cmd = vec!["uvx".to_string(), "mcp-server-fetch".to_string()];
        let parts = build_parts(Runner::Uvx, &cmd);
        assert_eq!(parts, vec!["uvx", "mcp-server-fetch"]);
    }

    #[test]
    fn test_build_parts_pipx() {
        let cmd = vec!["pipx".to_string(), "mcp-server-fetch".to_string()];
        let parts = build_parts(Runner::Pipx, &cmd);
        assert_eq!(parts, vec!["pipx", "run", "mcp-server-fetch"]);
    }

    #[test]
    fn test_build_parts_npx() {
        let cmd = vec![
            "npx".to_string(),
            "-y".to_string(),
            "mcp-server-fetch".to_string(),
        ];
        let parts = build_parts(Runner::Npx, &cmd);
        assert_eq!(parts, vec!["npx", "-y", "mcp-server-fetch"]);
    }

    #[test]
    fn test_build() {
        let cmd = vec!["uvx".to_string(), "mcp-server-fetch".to_string()];
        let entrypoint = build(Runner::Uvx, &cmd);
        assert_eq!(entrypoint, "ENTRYPOINT [\"uvx\",\"mcp-server-fetch\"]");
    }
}
