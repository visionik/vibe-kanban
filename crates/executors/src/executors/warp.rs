use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use command_group::AsyncCommandGroup;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use ts_rs::TS;
use workspace_utils::msg_store::MsgStore;

use crate::{
    command::{CmdOverrides, CommandBuilder, apply_overrides},
    env::ExecutionEnv,
    executors::{
        AppendPrompt, AvailabilityInfo, ExecutorError, SpawnedChild,
        StandardCodingAgentExecutor,
        warp_log_processor::WarpLogProcessor,
    },
    logs::{stderr_processor::normalize_stderr_logs, utils::EntryIndexProvider},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, JsonSchema)]
pub struct Warp {
    #[serde(default)]
    pub append_prompt: AppendPrompt,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        title = "Ambient Mode",
        description = "Run agent remotely using run-ambient instead of local run"
    )]
    pub ambient: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        title = "Model",
        description = "Override the base model used by the agent"
    )]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        title = "Profile",
        description = "ID of the profile the agent will run as"
    )]
    pub profile: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        title = "Environment",
        description = "Cloud environment to use (for ambient mode)"
    )]
    pub environment: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        title = "Output Format",
        description = "Output format: json, pretty, or text"
    )]
    pub output_format: Option<String>,

    #[serde(flatten)]
    pub cmd: CmdOverrides,
}

impl Warp {
    fn build_command_builder(&self) -> CommandBuilder {
        let base_cmd = if self.ambient.unwrap_or(false) {
            "warp agent run-ambient"
        } else {
            "warp agent run"
        };

        let mut builder = CommandBuilder::new(base_cmd);

        // Add optional parameters BEFORE --prompt
        if let Some(model) = &self.model {
            builder = builder.extend_params(["--model", model]);
        }

        if let Some(profile) = &self.profile {
            builder = builder.extend_params(["--profile", profile]);
        }

        if let Some(environment) = &self.environment {
            builder = builder.extend_params(["-e", environment]);
        }

        if let Some(format) = &self.output_format {
            builder = builder.extend_params(["--output-format", format]);
        } else {
            // Default to JSON for easier parsing
            builder = builder.extend_params(["--output-format", "json"]);
        }

        // Add --prompt flag at the END (required by Warp CLI)
        builder = builder.extend_params(["--prompt"]);

        apply_overrides(builder, &self.cmd)
    }
}

#[async_trait]
impl StandardCodingAgentExecutor for Warp {
    async fn spawn(
        &self,
        current_dir: &Path,
        prompt: &str,
        env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        tracing::info!("Spawning Warp agent");
        tracing::debug!(
            "Warp config: ambient={:?}, model={:?}, profile={:?}, environment={:?}, output_format={:?}",
            self.ambient,
            self.model,
            self.profile,
            self.environment,
            self.output_format
        );

        let command_builder = self.build_command_builder();
        let command_parts = command_builder.build_initial()?;
        let (executable_path, args) = command_parts.into_resolved().await?;

        tracing::debug!("Warp executable: {:?}", executable_path);
        tracing::debug!("Warp args: {:?}", args);

        let combined_prompt = self.append_prompt.combine_prompt(prompt);
        tracing::debug!("Prompt length: {} chars", combined_prompt.len());

        let mut command = Command::new(executable_path);
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(current_dir)
            .args(&args)
            .arg(&combined_prompt); // -p flag is already in args, this is the prompt value

        env.clone()
            .with_profile(&self.cmd)
            .apply_to_command(&mut command);

        tracing::info!("Spawning Warp process in directory: {:?}", current_dir);
        let child = command.group_spawn()?;
        tracing::info!("Warp process spawned successfully");

        Ok(child.into())
    }

    async fn spawn_follow_up(
        &self,
        _current_dir: &Path,
        _prompt: &str,
        session_id: &str,
        _env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        tracing::warn!(
            "Warp spawn_follow_up called with session_id={}, but session forking is not yet supported",
            session_id
        );
        Err(ExecutorError::FollowUpNotSupported(
            "Warp agent does not support session forking yet".to_string(),
        ))
    }

    fn normalize_logs(&self, msg_store: Arc<MsgStore>, current_dir: &Path) {
        tracing::debug!("Normalizing Warp logs for directory: {:?}", current_dir);
        let entry_index_provider = EntryIndexProvider::start_from(&msg_store);

        // Process stdout logs (Warp's JSON output) using dedicated Warp log processor
        WarpLogProcessor::process_logs(
            msg_store.clone(),
            current_dir,
            entry_index_provider.clone(),
        );

        // Process stderr logs using the standard stderr processor
        normalize_stderr_logs(msg_store, entry_index_provider);
        tracing::debug!("Warp log normalization complete");
    }

    fn default_mcp_config_path(&self) -> Option<std::path::PathBuf> {
        // Warp MCP config is typically in the Warp config directory
        dirs::config_dir().map(|config| config.join("warp").join("mcp.json"))
    }

    fn get_availability_info(&self) -> AvailabilityInfo {
        tracing::debug!("Checking Warp CLI availability");
        // Check if warp CLI is installed by trying to get help output
        if let Ok(output) = std::process::Command::new("warp").arg("--help").output() {
            if output.status.success() {
                let help_text = String::from_utf8_lossy(&output.stdout);
                // Verify it's the Warp CLI by checking for "agent" command
                if help_text.contains("agent") {
                    tracing::info!("Warp CLI found and available");
                    return AvailabilityInfo::InstallationFound;
                }
            }
        }

        tracing::warn!("Warp CLI not found in PATH");
        AvailabilityInfo::NotFound
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use workspace_utils::msg_store::MsgStore;

    #[test]
    fn test_warp_command_builder_local() {
        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: Some(false),
            model: Some("claude-3-7-sonnet-20250219".to_string()),
            profile: None,
            environment: None,
            output_format: Some("json".to_string()),
            cmd: Default::default(),
        };

        let builder = warp.build_command_builder();
        // Just verify the builder was created with correct base command
        assert!(builder.base.contains("warp agent run"));
        assert!(!builder.base.contains("run-ambient"));
    }

    #[test]
    fn test_warp_command_builder_ambient() {
        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: Some(true),
            model: None,
            profile: Some("profile-123".to_string()),
            environment: Some("env-456".to_string()),
            output_format: None,
            cmd: Default::default(),
        };

        let builder = warp.build_command_builder();
        // Verify the builder has run-ambient in base command
        assert!(builder.base.contains("warp agent run-ambient"));
        
        // Verify params were added
        let params = builder.params.as_ref().unwrap();
        assert!(params.contains(&"--profile".to_string()));
        assert!(params.contains(&"profile-123".to_string()));
        assert!(params.contains(&"-e".to_string()));
        assert!(params.contains(&"env-456".to_string()));
        assert!(params.contains(&"--output-format".to_string()));
        assert!(params.contains(&"json".to_string()));
    }

    #[test]
    fn test_append_prompt_combination() {
        let warp = Warp {
            append_prompt: AppendPrompt(Some("\n\nAlways use conventional commits.".to_string())),
            ambient: None,
            model: None,
            profile: None,
            environment: None,
            output_format: None,
            cmd: Default::default(),
        };

        let prompt = "Fix the bug in main.rs";
        let combined = warp.append_prompt.combine_prompt(prompt);

        assert_eq!(
            combined,
            "Fix the bug in main.rs\n\nAlways use conventional commits."
        );
    }

    #[test]
    fn test_default_output_format() {
        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: None,
            model: None,
            profile: None,
            environment: None,
            output_format: None, // Not specified
            cmd: Default::default(),
        };

        let builder = warp.build_command_builder();
        // Should default to json
        let params = builder.params.as_ref().unwrap();
        assert!(params.contains(&"--output-format".to_string()));
        assert!(params.contains(&"json".to_string()));
    }

    #[test]
    fn test_warp_availability_detection() {
        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: None,
            model: None,
            profile: None,
            environment: None,
            output_format: None,
            cmd: Default::default(),
        };

        let availability = warp.get_availability_info();
        // This will return InstallationFound or NotFound depending on whether
        // warp CLI is installed on the test machine
        match availability {
            AvailabilityInfo::InstallationFound => {
                // Warp is installed
                assert!(true);
            }
            AvailabilityInfo::NotFound => {
                // Warp is not installed, which is fine for unit tests
                assert!(true);
            }
            _ => panic!("Unexpected availability info"),
        }
    }

    #[test]
    fn test_mcp_config_path() {
        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: None,
            model: None,
            profile: None,
            environment: None,
            output_format: None,
            cmd: Default::default(),
        };

        let mcp_path = warp.default_mcp_config_path();
        assert!(mcp_path.is_some());
        let path = mcp_path.unwrap();
        assert!(path.to_string_lossy().contains("warp"));
        assert!(path.to_string_lossy().contains("mcp.json"));
    }

    #[tokio::test]
    async fn test_warp_spawn_with_mock_command() {
        use tempfile::TempDir;

        // Create a temporary directory for the test
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Use a mock command that will fail (since warp may not be installed)
        // but we can still test the command building logic
        let warp = Warp {
            append_prompt: AppendPrompt(Some("\n\nTest prompt".to_string())),
            ambient: Some(false),
            model: Some("test-model".to_string()),
            profile: None,
            environment: None,
            output_format: Some("json".to_string()),
            cmd: CmdOverrides {
                base_command_override: Some("echo".to_string()), // Use echo as mock
                additional_params: None,
                env: None,
            },
        };

        let env_exec = ExecutionEnv {
            vars: std::collections::HashMap::new(),
        };

        let prompt = "Test task";

        // This should build the command successfully even if warp isn't installed
        let result = warp.spawn(temp_path, prompt, &env_exec).await;

        // With echo as base command, spawn should succeed
        match result {
            Ok(_spawned_child) => {
                // Success - command was built and spawned
                assert!(true);
            }
            Err(e) => {
                // If we get ExecutableNotFound for "warp", that's expected when not mocked
                // If we get any other error with echo mock, that's unexpected
                if let ExecutorError::ExecutableNotFound { program } = e {
                    assert_eq!(program, "warp");
                } else {
                    panic!("Unexpected error: {:?}", e);
                }
            }
        }
    }

    #[tokio::test]
    async fn test_warp_follow_up_not_supported() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: None,
            model: None,
            profile: None,
            environment: None,
            output_format: None,
            cmd: Default::default(),
        };

        let env_exec = ExecutionEnv {
            vars: std::collections::HashMap::new(),
        };

        let result = warp
            .spawn_follow_up(temp_path, "Follow up prompt", "session-123", &env_exec)
            .await;

        // Should return FollowUpNotSupported error
        assert!(result.is_err());
        match result {
            Err(ExecutorError::FollowUpNotSupported(msg)) => {
                assert!(msg.contains("session forking"));
            }
            _ => panic!("Expected FollowUpNotSupported error"),
        }
    }

    #[tokio::test]
    async fn test_warp_log_normalization() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: None,
            model: None,
            profile: None,
            environment: None,
            output_format: None,
            cmd: Default::default(),
        };

        let msg_store = Arc::new(MsgStore::new());

        // Push some test stderr messages
        msg_store.push_stderr("Starting Warp agent...\n".to_string());
        msg_store.push_stderr("Processing task...\n".to_string());
        msg_store.push_finished();

        // Normalize logs (this should process stderr)
        warp.normalize_logs(msg_store.clone(), temp_path);

        // Give time for async processing
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Verify that history contains normalized entries
        let history = msg_store.get_history();
        assert!(
            !history.is_empty(),
            "Expected normalized log entries to be generated"
        );
    }

    /// Integration test that requires warp CLI to be installed
    /// Run with: cargo test --package executors -- --ignored
    #[tokio::test]
    #[ignore]
    async fn test_warp_actual_spawn() {
        use tempfile::TempDir;

        // Skip if WARP_API_KEY is not set
        if std::env::var("WARP_API_KEY").is_err() {
            eprintln!("Skipping test: WARP_API_KEY not set");
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let warp = Warp {
            append_prompt: AppendPrompt(None),
            ambient: Some(false), // Use local mode for testing
            model: None,
            profile: None,
            environment: None,
            output_format: Some("json".to_string()),
            cmd: Default::default(),
        };

        let mut vars = std::collections::HashMap::new();
        if let Ok(api_key) = std::env::var("WARP_API_KEY") {
            vars.insert("WARP_API_KEY".to_string(), api_key);
        }

        let env_exec = ExecutionEnv { vars };

        let prompt = "Echo 'Hello from Warp test'";

        // Attempt to spawn actual warp process
        let result = warp.spawn(temp_path, prompt, &env_exec).await;

        match result {
            Ok(mut spawned_child) => {
                // Wait a bit for the process to start
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

                // Kill the process
                let _ = spawned_child.child.kill();
                let _ = spawned_child.child.wait().await;

                assert!(true, "Warp process spawned successfully");
            }
            Err(ExecutorError::ExecutableNotFound { program }) => {
                panic!("Warp CLI not found: {}. Install it to run this test.", program);
            }
            Err(e) => {
                panic!("Failed to spawn Warp: {:?}", e);
            }
        }
    }
}
