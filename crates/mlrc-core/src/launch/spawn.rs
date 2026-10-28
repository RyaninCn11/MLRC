//! Starting the game.

use std::process::{Command, Stdio};

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::launch::LaunchPlan;

/// Runs the game and returns its exit code unchanged.
///
/// Output is inherited rather than piped: a pipe would lose colour, add buffering,
/// and force us to decode a console stream that mixes UTF-8 and the legacy code
/// page. The launcher has nothing useful to add to the game's own output.
pub fn spawn(plan: &LaunchPlan) -> MlrcResult<i32> {
    let mut command = Command::new(&plan.java);
    command
        .args(&plan.args)
        .arg(&plan.main_class)
        .args(&plan.game_args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = command.spawn().map_err(|err| {
        MlrcError::new(
            ErrorCode::Io,
            format!("cannot start {}: {err}", plan.java.display()),
        )
    })?;

    let status = child.wait()?;
    Ok(status.code().unwrap_or(-1))
}
