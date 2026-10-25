//! Pi's `killTrackedDetachedChildren` (`src/utils/shell.ts`), which kills
//! every running command's tree on shutdown. It reaches every call in the
//! process, so it has a test binary of its own. Unix only: the check reads
//! the killed processes' pids.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bake_agent::AgentToolUpdateCallback;
use bake_ai::UserContentBlock;
use bake_coding_agent::tools::bash::{
    BashToolOptions, create_bash_tool, kill_tracked_detached_children,
};
use serde_json::json;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn process_gone(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: signal 0 only checks that the pid exists.
        unsafe { libc::kill(pid, 0) == -1 }
    }
}

/// Kills a running command's shell and grandchild; the call then settles
/// with the shell's signal exit, as Pi's does.
#[tokio::test]
async fn kill_tracked_detached_children_ends_running_commands() {
    let mut random = [0; 16];
    getrandom::fill(&mut random).unwrap();
    let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let dir = TempDir(std::env::temp_dir().join(format!("bake-bash-shutdown-{id}")));
    fs::create_dir(&dir.0).unwrap();
    let pid_file = dir.0.join("tree.pid");
    let tool = create_bash_tool(
        &dir.0,
        BashToolOptions {
            spill_directory: Some(dir.0.clone()),
            ..BashToolOptions::default()
        },
    );
    let command = format!("sleep 30 & echo $$ $! > '{}'; sleep 30", pid_file.display());
    let call = tokio::spawn((tool.execute)(
        "call".to_owned(),
        json!({ "command": command }),
        None,
        AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let pids: Vec<i32> = loop {
        let pids: Vec<i32> = fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect();
        if pids.len() == 2 {
            break pids;
        }
        assert!(
            Instant::now() < deadline,
            "the command never wrote its pids"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    kill_tracked_detached_children();
    let result = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect();
    assert_eq!(text, ["(no output)\n\nCommand exited with code 137"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pids.iter().all(|pid| process_gone(*pid)) {
        assert!(Instant::now() < deadline, "processes {pids:?} survived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A second call finds nothing tracked.
    kill_tracked_detached_children();
}
