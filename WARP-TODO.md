# Warp Integration Analysis & TODO

## Overview

This fork adds Warp agent support to vibe-kanban. This document analyzes the current implementation compared to the Claude executor and identifies gaps that need to be addressed.

---

## What's Implemented Correctly

### 1. Executor Core (`crates/executors/src/executors/warp.rs` - 527 lines)

| Feature | Status | Notes |
|---------|--------|-------|
| Config struct | Complete | `append_prompt`, `ambient`, `model`, `profile`, `environment`, `output_format`, `cmd` |
| Command building | Complete | Properly orders flags, defaults to JSON output |
| `spawn()` | Complete | Process spawning with proper stdio handling |
| `normalize_logs()` | Complete | Uses dedicated `WarpLogProcessor` |
| `default_mcp_config_path()` | Complete | Returns `~/.config/warp/mcp.json` |
| `get_availability_info()` | Complete | Checks `warp --help` for "agent" command |
| Unit tests | Complete | 15 tests covering core functionality |

### 2. Log Processor (`crates/executors/src/executors/warp_log_processor.rs` - 353 lines)

| Feature | Status | Notes |
|---------|--------|-------|
| `WarpJson` enum | Complete | 4 message types: `System`, `Agent`, `ToolCall`, `ToolResult` |
| Session ID extraction | Complete | From `conversation_id` field |
| Tool call tracking | Complete | Maps tool calls to results via `tool_map` |
| `run_command` handling | Complete | Extracts command, exit_code, output |
| `read_files` handling | Complete | Shows abbreviated file paths |
| Tool result updates | Complete | Updates entry status to Success/Failed |

### 3. Frontend Integration

| Component | Status | Location |
|-----------|--------|----------|
| `AgentIcon.tsx` | Complete | `getAgentName()` returns "Warp", loads icons |
| Icons | Complete | `assets/agents/warp-dark.svg`, `warp-light.svg` |

### 4. Configuration & Types

| File | Status | Notes |
|------|--------|-------|
| `shared/schemas/warp.json` | Complete | Full schema with all options |
| `mod.rs` registration | Complete | `Warp` variant in `CodingAgent` enum |
| Capabilities | Correct | Returns `vec![]` (no SessionFork) |

---

## Critical Gaps

### 1. No Session Forking (Blocking for multi-turn conversations)

**Location:** `warp.rs:152-166`

```rust
async fn spawn_follow_up(...) -> Result<SpawnedChild, ExecutorError> {
    tracing::warn!("Warp spawn_follow_up called...");
    Err(ExecutorError::FollowUpNotSupported(
        "Warp agent does not support session forking yet".to_string(),
    ))
}
```

**Impact:** Users cannot continue conversations across task boundaries.

**Fix Required:** Implement session continuation if Warp CLI supports `--resume` or similar flags.

### 2. Limited Message Type Support

**Warp (4 types):**
- `System`, `Agent`, `ToolCall`, `ToolResult`

**Claude (12+ types):**
- `System`, `Assistant`, `User`, `ToolUse`, `ToolResult`, `StreamEvent` (with 6 sub-types), `Result`, `ApprovalResponse`, `ControlRequest`, `ControlResponse`, `ControlCancelRequest`, `Unknown`

**Missing in Warp:**
- Streaming events (`MessageStart`, `ContentBlockDelta`, etc.)
- Thinking/reasoning content
- User messages
- Approval responses
- Control protocol messages

### 3. No Control Protocol

Claude has full `ProtocolPeer` implementation (`claude.rs:282-308`) for:
- Interactive hooks
- Permission mode changes
- Approval system integration

Warp has none of this infrastructure.

### 4. No Approval System

Claude integrates with `ExecutorApprovalService`:
```rust
// claude.rs:162-164
fn use_approvals(&mut self, approvals: Arc<dyn ExecutorApprovalService>) {
    self.approvals_service = Some(approvals);
}
```

Warp does not implement `use_approvals()`.

---

## Medium Priority Issues

### 1. Tool Type Coverage

**Well-handled tools:**
- `run_command` - Full handling with exit_code, output
- `read_files` - Path extraction and display

**Missing tool-specific handling:**

| Tool | Claude Support | Warp Support |
|------|----------------|--------------|
| `Read`, `Edit`, `Write`, `MultiEdit` | Full structured parsing | Generic JSON only |
| `Glob`, `Grep` | Search pattern extraction | Generic JSON only |
| `WebFetch`, `WebSearch` | URL extraction | Generic JSON only |
| `Task` | Description extraction | Generic JSON only |
| `TodoWrite`, `TodoRead` | Todo list formatting | Not handled |
| `Thinking` content | Full support | Not handled |

### 2. Streaming Support

Claude handles streaming via `StreamingMessageState`:
- `ContentBlockStart`, `ContentBlockDelta`, `ContentBlockStop`
- Progressive message building

Warp only processes complete JSON lines.

### 3. MCP Config Path

Warp uses `~/.config/warp/mcp.json` which is reasonable, but should verify this is the correct Warp CLI location.

---

## Code Size Comparison

| Executor | Log Processor | Total | Ratio |
|----------|---------------|-------|-------|
| Claude | ~2,357 lines | ~2,357 | 100% |
| Warp | 527 + 353 = 880 lines | 880 | 37% |

---

## TODO: Recommended Fixes

### Phase 1: Critical (Required for basic functionality)

- [ ] **Investigate Warp CLI session support**
  - Check if `warp agent run --resume <session_id>` exists
  - If available, implement `spawn_follow_up()` properly
  - If not, document the limitation clearly

- [ ] **Add error handling for Warp CLI not installed**
  - Current: Only logs warning
  - Suggested: Return descriptive error message to user

### Phase 2: High Priority (Improve UX)

- [ ] **Extend `WarpJson` enum** for more message types:
  ```rust
  pub enum WarpJson {
      // Existing...
      Thinking { text: String },  // If Warp supports this
      FileOperation { ... },       // If Warp has file tools
  }
  ```

- [ ] **Add tool-specific content generation** for common tools:
  - File read/write operations
  - Search/grep operations

### Phase 3: Medium Priority (Feature parity)

- [ ] **Streaming support** (if Warp CLI supports it)
- [ ] **Approval system integration** (if Warp has interactive mode)

### Phase 4: Low Priority (Polish)

- [ ] **Add setup helper** if Warp requires authentication flow
- [ ] **Update seed data** to include Warp in default configurations

---

## Summary

The Warp integration is **~37% complete** compared to Claude Code. The core execution flow works, but lacks:

| Category | Status |
|----------|--------|
| Basic execution | Complete |
| Process management | Complete |
| Configuration | Complete |
| Frontend support | Complete |
| Session continuity | Missing |
| Message type coverage | Partial (4 of 12+) |
| Tool tracking | Partial (2 of 20+) |
| Control protocol | Missing |
| Approval system | Missing |

**The implementation is usable for single-turn tasks** but needs session forking for proper multi-turn conversation support.
