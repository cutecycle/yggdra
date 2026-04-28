//! Asynchronous streaming task for draining child process stdout and sending output through mpsc channels.

use tokio::io::{AsyncBufReadExt, BufReader};

/// Spawn a background task to drain stdout from a child process
/// and send lines through an mpsc channel.
///
/// This function reads from the child process's stdout line-by-line,
/// sending each line through the provided mpsc sender. The function
/// blocks until EOF is reached or an error occurs, then waits for the
/// process to finish and returns its exit code.
///
/// # Arguments
/// * `mut child` - The tokio child process
/// * `tx` - The mpsc channel sender for output lines
///
/// # Returns
/// * `Ok(exit_code)` - Process exit code if successful
/// * `Err(e)` - Error if stdout capture failed or process wait failed
pub async fn spawn_panel_stream_task(
    mut child: tokio::process::Child,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<i32, Box<dyn std::error::Error>> {
    // Extract stdout
    let stdout = child.stdout.take()
        .ok_or("Failed to capture stdout")?;
    
    // Create a BufReader to read lines
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    
    // Read lines until EOF
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF
            Ok(_) => {
                // Send the line (ignore if receiver dropped)
                let _ = tx.send(line.clone());
            }
            Err(e) => {
                crate::dlog!("Error reading from process: {}", e);
                break;
            }
        }
    }
    
    // Wait for process to finish and get exit code
    let status = child.wait().await?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;

    #[tokio::test]
    async fn test_simple_echo_command() {
        let child = Command::new("echo")
            .arg("hello")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn echo");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let task = spawn_panel_stream_task(child, tx);
        let exit_code = task.await.expect("Task failed");

        // Collect output
        let mut output = Vec::new();
        while let Some(line) = rx.recv().await {
            output.push(line);
        }

        assert_eq!(exit_code, 0, "Command should succeed");
        assert_eq!(output.len(), 1, "Should receive one line");
        assert_eq!(output[0].trim(), "hello", "Output should match");
    }

    #[tokio::test]
    async fn test_multiline_output() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("echo line1; echo line2; echo line3")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn shell");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let task = spawn_panel_stream_task(child, tx);
        let exit_code = task.await.expect("Task failed");

        let mut output = Vec::new();
        while let Some(line) = rx.recv().await {
            output.push(line.trim().to_string());
        }

        assert_eq!(exit_code, 0, "Command should succeed");
        assert_eq!(output.len(), 3, "Should receive three lines");
        assert_eq!(output[0], "line1");
        assert_eq!(output[1], "line2");
        assert_eq!(output[2], "line3");
    }

    #[tokio::test]
    async fn test_process_exit_code() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 42")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn shell");

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let task = spawn_panel_stream_task(child, tx);
        let exit_code = task.await.expect("Task failed");

        assert_eq!(exit_code, 42, "Should capture exit code 42");
    }

    #[tokio::test]
    async fn test_receiver_dropped() {
        // When receiver is dropped, sending should not panic
        let child = Command::new("sh")
            .arg("-c")
            .arg("echo output")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn shell");

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx); // Drop receiver before task runs

        let task = spawn_panel_stream_task(child, tx);
        let exit_code = task.await.expect("Task failed");

        assert_eq!(exit_code, 0, "Should still complete without panic");
    }

    #[tokio::test]
    async fn test_invalid_command() {
        // Try to spawn a command that doesn't exist
        let result = Command::new("/nonexistent/binary/that/does/not/exist")
            .stdout(std::process::Stdio::piped())
            .spawn();

        // This should fail at spawn time, not in the task
        assert!(result.is_err(), "Should fail to spawn invalid command");
    }

    #[tokio::test]
    async fn test_long_line_output() {
        let long_string = "a".repeat(10000);
        let child = Command::new("echo")
            .arg(&long_string)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn echo");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let task = spawn_panel_stream_task(child, tx);
        let exit_code = task.await.expect("Task failed");

        let mut output = Vec::new();
        while let Some(line) = rx.recv().await {
            output.push(line);
        }

        assert_eq!(exit_code, 0);
        assert_eq!(output.len(), 1);
        assert!(output[0].contains(&long_string));
    }

    #[tokio::test]
    async fn test_stderr_not_captured() {
        // stderr should not be captured (only stdout)
        let child = Command::new("sh")
            .arg("-c")
            .arg("echo stdout; echo stderr >&2")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn shell");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let task = spawn_panel_stream_task(child, tx);
        let exit_code = task.await.expect("Task failed");

        let mut output = Vec::new();
        while let Some(line) = rx.recv().await {
            output.push(line.trim().to_string());
        }

        assert_eq!(exit_code, 0);
        assert_eq!(output.len(), 1);
        assert_eq!(output[0], "stdout");
    }
}
