#![cfg_attr(windows, windows_subsystem = "windows")]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use hd2_updater::{
    acquire_install_lock, apply_staged_release, commit_install, extract_release_archive,
    recover_incomplete_updates, rollback_install, sha256_file, show_update_failure,
    validate_digest, validate_update_transaction_dir, wait_for_parent_process, wait_for_process,
    UpdateError, MAX_ARCHIVE_BYTES,
};

const READY_FILE: &str = "helper-ready";
const ERROR_FILE: &str = "helper-error";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

struct ApplyArgs {
    install_dir: PathBuf,
    transaction_dir: PathBuf,
    archive: PathBuf,
    expected_sha256: String,
    release_tag: String,
    parent_pid: u32,
    ready_file: PathBuf,
    health_file: PathBuf,
}

fn main() {
    if let Err((message, transaction_dir, show_dialog)) = run() {
        if let Some(directory) = transaction_dir {
            let _ = write_marker(&directory.join(ERROR_FILE), message.as_bytes());
        }
        if show_dialog {
            show_update_failure(&message);
        }
    }
}

fn run() -> Result<(), (String, Option<PathBuf>, bool)> {
    let args = parse_args().map_err(|error| (error, None, true))?;
    let result = apply_update(&args);
    result.map_err(|error| {
        (
            error.to_string(),
            Some(args.transaction_dir.clone()),
            args.transaction_dir.join(READY_FILE).exists(),
        )
    })
}

fn parse_args() -> Result<ApplyArgs, String> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("apply")) {
        return Err("更新程序参数无效".to_string());
    }
    let install_dir = next_path(&mut args, "程序目录")?;
    let transaction_dir = next_path(&mut args, "更新事务目录")?;
    let archive = next_path(&mut args, "更新包路径")?;
    let expected_sha256 = next_string(&mut args, "SHA-256")?;
    let release_tag = next_string(&mut args, "版本标签")?;
    let parent_pid = next_string(&mut args, "主程序进程号")?
        .parse::<u32>()
        .map_err(|_| "主程序进程号无效".to_string())?;
    let ready_file = next_path(&mut args, "updater 就绪标记")?;
    let health_file = next_path(&mut args, "新版本启动标记")?;
    if args.next().is_some() {
        return Err("更新程序参数过多".to_string());
    }
    validate_digest(&expected_sha256).map_err(|error| error.to_string())?;
    Ok(ApplyArgs {
        install_dir,
        transaction_dir,
        archive,
        expected_sha256,
        release_tag,
        parent_pid,
        ready_file,
        health_file,
    })
}

fn next_path(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    label: &str,
) -> Result<PathBuf, String> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("缺少{label}"))
}

fn next_string(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    label: &str,
) -> Result<String, String> {
    args.next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| format!("{label}无效"))
}

fn apply_update(args: &ApplyArgs) -> Result<(), UpdateError> {
    let install_dir = args.install_dir.canonicalize()?;
    let transaction_dir = validate_update_transaction_dir(&install_dir, &args.transaction_dir)?;
    let archive = args.archive.canonicalize()?;
    if archive != transaction_dir.join("package.zip") {
        return Err(UpdateError::Install("更新包路径无效".to_string()));
    }
    for (marker, expected_name) in [
        (&args.ready_file, READY_FILE),
        (&args.health_file, "health"),
    ] {
        let parent = marker
            .parent()
            .ok_or_else(|| UpdateError::Install("更新状态文件路径无效".to_string()))?
            .canonicalize()?;
        if parent != transaction_dir
            || marker.file_name().and_then(|name| name.to_str()) != Some(expected_name)
        {
            return Err(UpdateError::Install("更新状态文件路径无效".to_string()));
        }
    }
    let helper = std::env::current_exe()?.canonicalize()?;
    let expected_helper = transaction_dir
        .join("preflight")
        .join("hd2-armor-desk-updater.exe")
        .canonicalize()?;
    if helper != expected_helper {
        return Err(UpdateError::Install(
            "更新程序不在事务暂存目录内".to_string(),
        ));
    }
    let archive_meta = fs::metadata(&archive)?;
    if !archive_meta.is_file() || archive_meta.len() == 0 || archive_meta.len() > MAX_ARCHIVE_BYTES
    {
        return Err(UpdateError::Install("更新包大小异常".to_string()));
    }
    let _lock = acquire_install_lock(&install_dir)?;
    let parent = wait_for_parent_process(args.parent_pid)?;
    write_marker(&args.ready_file, b"ready")?;
    wait_for_process(parent)?;

    if sha256_file(&archive)? != args.expected_sha256.to_ascii_lowercase() {
        return Err(UpdateError::Install(
            "主程序退出后更新包 SHA-256 二次校验失败".to_string(),
        ));
    }
    recover_incomplete_updates(&install_dir)?;
    let staged_dir = transaction_dir.join("new-files");
    extract_release_archive(&archive, &staged_dir, &args.release_tag)?;
    apply_staged_release(
        &install_dir,
        &staged_dir,
        &transaction_dir,
        &args.release_tag,
    )?;

    let mut child = match launch_new_version(&install_dir, &args.health_file, &args.release_tag) {
        Ok(child) => child,
        Err(error) => {
            return Err(rollback_after_failure(
                &install_dir,
                &transaction_dir,
                error,
            ))
        }
    };
    if let Err(error) = commit_install(&transaction_dir) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(rollback_after_failure(
            &install_dir,
            &transaction_dir,
            error,
        ));
    }
    Ok(())
}

fn rollback_after_failure(
    install_dir: &Path,
    transaction_dir: &Path,
    error: UpdateError,
) -> UpdateError {
    match rollback_install(install_dir, transaction_dir) {
        Ok(()) => UpdateError::Install(format!("更新失败，已恢复旧版本：{error}")),
        Err(rollback_error) => UpdateError::Install(format!(
            "更新失败：{error}；自动回滚也失败：{rollback_error}。备份和更新暂存区已保留"
        )),
    }
}

fn launch_new_version(
    install_dir: &Path,
    health_file: &Path,
    release_tag: &str,
) -> Result<Child, UpdateError> {
    let executable = install_dir.join("hd2-armor-desk.exe");
    let mut child = Command::new(&executable)
        .arg("--hd2-update-health")
        .arg(health_file)
        .current_dir(install_dir)
        .spawn()
        .map_err(|error| UpdateError::Install(format!("无法启动新版本：{error}")))?;
    let started = match wait_for_health(&mut child, health_file, release_tag) {
        Ok(started) => started,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    if !started {
        let _ = child.kill();
        let _ = child.wait();
        return Err(UpdateError::Install(
            "新版本未能在限定时间内完成启动".to_string(),
        ));
    }
    Ok(child)
}

fn wait_for_health(
    child: &mut Child,
    health_file: &Path,
    release_tag: &str,
) -> Result<bool, UpdateError> {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(health_file) {
            return Ok(contents == release_tag);
        }
        if child.try_wait()?.is_some() {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn write_marker(path: &Path, contents: &[u8]) -> Result<(), UpdateError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}
