//! 隔离测试：Rust spawn `adb shell` 的输出转发问题
use std::process::{Command, Stdio};

fn adb() -> String {
    std::env::var("SCREENLINK_ADB").unwrap_or_else(|_| {
        std::env::var("LOCALAPPDATA")
            .map(|l| format!("{l}\\Android\\Sdk\\platform-tools\\adb.exe"))
            .unwrap_or_else(|_| "adb".into())
    })
}

fn try_spawn(name: &str, no_window: bool, stdin_mode: Stdio) {
    let mut cmd = Command::new(adb());
    cmd.args(["-s", "emulator-5554", "shell", "echo", "HELLO_FROM_SHELL"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(stdin_mode);
    if no_window {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().expect("spawn failed");
    println!(
        "{name}: code={:?} stdout={:?} stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

fn main() {
    try_spawn("A plain", false, Stdio::null());
    try_spawn("B no_window", true, Stdio::null());
    try_spawn("C no_window+piped_stdin", true, Stdio::piped());
    try_spawn("D plain+piped_stdin", false, Stdio::piped());

    // 长时 shell：spawn 后 3s 检查 stdout 是否有数据
    for (name, nw) in [("E long plain", false), ("F long no_window", true)] {
        let mut cmd = Command::new(adb());
        cmd.args(["-s", "emulator-5554", "shell", "sleep 2; echo LONG_OK"]);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::piped());
        if nw {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().expect("spawn failed");
        // 保持 stdin 打开
        let _stdin = child.stdin.take();
        let start = std::time::Instant::now();
        let mut buf = Vec::new();
        use std::io::Read;
        let mut out = child.stdout.take().unwrap();
        let _ = out.read_to_end(&mut buf);
        println!(
            "{name}: took {:?} out={:?}",
            start.elapsed(),
            String::from_utf8_lossy(&buf).trim()
        );
    }
}
