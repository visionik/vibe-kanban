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

        let mut builder = CommandBuilder::new(base_cmd).params(["-p"]);

        // Add optional parameters
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
        let command_builder = self.build_command_builder();
        let command_parts = command_builder.build_initial()?;
        let (executable_path, args) = command_parts.into_resolved().await?;

        let combined_prompt = self.append_prompt.combine_prompt(prompt);

        let mut command = Command::new(executable_path);
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(current_dir)
            .args(&args)
            .arg(&combined_prompt);

        env.clone()
            .with_profile(&self.cmd)
            .apply_to_command(&mut command);

        let child = command.group_spawn()?;

        Ok(child.into())
    }

    async fn spawn_follow_up(
        &self,
        _current_dir: &Path,
        _prompt: &str,
        _session_id: &str,
        _env: &ExecutionEnv,
    ) -> Result<SpawnedChild, ExecutorError> {
        Err(ExecutorError::FollowUpNotSupported(
            "Warp agent does not support session forking yet".to_string(),
        ))
    }

    fn normalize_logs(&self, msg_store: Arc<MsgStore>, _current_dir: &Path) {
        let entry_index_provider = EntryIndexProvider::start_from(&msg_store);

        // For now, use basic stderr normalization
        // TODO: Add JSON parsing if output_format is json
        normalize_stderr_logs(msg_store, entry_index_provider);
    }

    fn default_mcp_config_path(&self) -> Option<std::path::PathBuf> {
        // Warp MCP config is typically in the Warp config directory
        dirs::config_dir().map(|config| config.join("warp").join("mcp.json"))
    }

    fn get_availability_info(&self) -> AvailabilityInfo {
        // Check if warp CLI is installed by trying to run it
        if let Ok(output) = std::process::Command::new("warp").arg("--version").output() {
            if output.status.success() {
                return AvailabilityInfo::InstallationFound;
            }
        }

        AvailabilityInfo::NotFound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
