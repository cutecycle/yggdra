// yggdra-shell binary entry point
// Separate binary definition to avoid Cargo "multiple build targets" warning
// Both yggdra and yggdra-shell are now functionally identical (shell-only mode is hardcoded)
// This wrapper delegates to yggdra while maintaining separate source files

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    
    let current_exe = std::env::current_exe()?;
    let bin_dir = current_exe.parent().expect("No parent directory");
    let yggdra_path = bin_dir.join("yggdra");
    
    // If yggdra exists alongside yggdra-shell, use it; otherwise rely on PATH
    let cmd = if yggdra_path.exists() {
        yggdra_path
    } else {
        std::path::PathBuf::from("yggdra")
    };
    
    // Replace this process with yggdra
    std::process::Command::new(&cmd)
        .args(std::env::args().skip(1))
        .exec();
    
    unreachable!()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("yggdra-shell is only supported on Unix-like systems");
    std::process::exit(1);
}
