//! GitHub Release update discovery and verified package downloads.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

pub const RELEASES_PAGE_URL: &str = "https://github.com/lona-cn/hd2-sav-editor/releases";
const LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/lona-cn/hd2-sav-editor/releases/latest";
const USER_AGENT: &str = "hd2-armor-desk-update-checker";
const MAX_RELEASE_BYTES: u64 = 256 * 1024 * 1024;

fn get_request(url: &str, timeout: Duration) -> ureq::Request {
    let is_loopback = url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:");
    ureq::AgentBuilder::new()
        .try_proxy_from_env(!is_loopback)
        .build()
        .get(url)
        .timeout(timeout)
}

pub const CURRENT_RELEASE_TAG: &str = match option_env!("HD2_RELEASE_TAG") {
    Some(tag) => tag,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePackage {
    pub name: String,
    pub download_url: String,
    pub size: u64,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag: String,
    pub name: String,
    pub page_url: String,
    pub package: ReleasePackage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheck {
    UpToDate(ReleaseInfo),
    Available(ReleaseInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate(ReleaseInfo),
    Available(ReleaseInfo),
    Downloading(ReleaseInfo),
    Downloaded(ReleaseInfo, DownloadReceipt),
    Installing(ReleaseInfo),
    CheckFailed(String),
    DownloadFailed(String),
    InstallFailed(String),
}

impl UpdateState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            UpdateState::Checking
                | UpdateState::Downloading(_)
                | UpdateState::Downloaded(_, _)
                | UpdateState::Installing(_)
        )
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

pub fn check_for_update() -> Result<UpdateCheck, String> {
    let result = check_for_update_at(LATEST_RELEASE_API_URL, CURRENT_RELEASE_TAG)?;
    let release = match &result {
        UpdateCheck::UpToDate(release) | UpdateCheck::Available(release) => release,
    };
    validate_release_urls(release)?;
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReceipt {
    pub path: PathBuf,
    pub transaction_dir: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

pub fn download_release(release: &ReleaseInfo) -> Result<DownloadReceipt, String> {
    validate_release_urls(release)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("无法定位当前程序：{error}"))?;
    let install_dir = executable
        .parent()
        .ok_or_else(|| "当前程序路径没有父目录".to_string())?;
    download_release_in(release, install_dir)
}

fn download_release_in(
    release: &ReleaseInfo,
    install_dir: &Path,
) -> Result<DownloadReceipt, String> {
    let install_dir = install_dir
        .canonicalize()
        .map_err(|error| format!("无法定位程序目录：{error}"))?;
    let lock = hd2_updater::acquire_install_lock(&install_dir)
        .map_err(|error| format!("无法准备更新目录：{error}"))?;
    hd2_updater::recover_incomplete_updates(&install_dir)
        .map_err(|error| format!("发现未完成的更新且无法恢复：{error}"))?;
    hd2_updater::cleanup_update_staging(&install_dir)
        .map_err(|error| format!("无法清理上次更新暂存文件：{error}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let transaction_id = format!("{}-{timestamp:x}", std::process::id());
    let transaction_dir = hd2_updater::create_update_transaction_dir(&install_dir, &transaction_id)
        .map_err(|error| format!("无法创建程序目录下的更新暂存区：{error}"))?;
    drop(lock);

    let destination = transaction_dir.join("package.zip");
    match download_release_to_inner(release, &destination) {
        Ok(mut receipt) => {
            receipt.transaction_dir = transaction_dir;
            Ok(receipt)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&transaction_dir);
            Err(error)
        }
    }
}

pub(crate) fn launch_update_helper(
    release: &ReleaseInfo,
    receipt: &DownloadReceipt,
) -> Result<(), String> {
    let result = launch_update_helper_inner(release, receipt);
    if result.is_err() {
        let _ = fs::remove_dir_all(&receipt.transaction_dir);
    }
    result
}

fn launch_update_helper_inner(
    release: &ReleaseInfo,
    receipt: &DownloadReceipt,
) -> Result<(), String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("无法定位当前程序：{error}"))?;
    let install_dir = executable
        .parent()
        .ok_or_else(|| "当前程序路径没有父目录".to_string())?
        .canonicalize()
        .map_err(|error| format!("无法定位程序目录：{error}"))?;
    let transaction_dir =
        hd2_updater::validate_update_transaction_dir(&install_dir, &receipt.transaction_dir)
            .map_err(|error| format!("更新暂存目录校验失败：{error}"))?;
    let archive = receipt
        .path
        .canonicalize()
        .map_err(|error| format!("更新包路径无效：{error}"))?;
    if archive != transaction_dir.join("package.zip")
        || hd2_updater::sha256_file(&archive)
            .map_err(|error| format!("无法再次读取更新包：{error}"))?
            != receipt.sha256
    {
        return Err("更新包在校验后发生变化，已取消安装".to_string());
    }

    let preflight_dir = transaction_dir.join("preflight");
    hd2_updater::extract_updater_binary(&archive, &preflight_dir, &release.tag)
        .map_err(|error| format!("更新包内容无效：{error}"))?;
    let helper = preflight_dir.join("hd2-armor-desk-updater.exe");
    let ready_file = transaction_dir.join("helper-ready");
    let error_file = transaction_dir.join("helper-error");
    let health_file = transaction_dir.join("health");
    let mut child = Command::new(&helper)
        .arg("apply")
        .arg(&install_dir)
        .arg(&transaction_dir)
        .arg(&archive)
        .arg(&receipt.sha256)
        .arg(&release.tag)
        .arg(std::process::id().to_string())
        .arg(&ready_file)
        .arg(&health_file)
        .current_dir(&install_dir)
        .spawn()
        .map_err(|error| format!("无法启动更新程序：{error}"))?;

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if ready_file.is_file() {
            return Ok(());
        }
        if error_file.is_file() {
            let message = fs::read_to_string(&error_file)
                .unwrap_or_else(|error| format!("更新程序启动失败：{error}"));
            let _ = child.kill();
            let _ = child.wait();
            return Err(message);
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("无法检查更新程序状态：{error}"))?
        {
            let message = fs::read_to_string(&error_file)
                .unwrap_or_else(|_| format!("更新程序提前退出（{status}）"));
            return Err(message);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("更新程序未能及时启动，原版本保持不变".to_string());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn validate_release_urls(release: &ReleaseInfo) -> Result<(), String> {
    let trusted_prefix = "https://github.com/lona-cn/hd2-sav-editor/releases/";
    if !release.page_url.starts_with(trusted_prefix)
        || !release.package.download_url.starts_with(trusted_prefix)
    {
        return Err("GitHub 返回了不受信任的发布地址，已拒绝访问".to_string());
    }
    Ok(())
}

fn download_release_to_inner(
    release: &ReleaseInfo,
    destination: &Path,
) -> Result<DownloadReceipt, String> {
    if release.package.size == 0 || release.package.size > MAX_RELEASE_BYTES {
        return Err(format!(
            "发布包大小异常（{} 字节），已拒绝下载",
            release.package.size
        ));
    }
    if destination.exists() {
        return Err(format!(
            "程序目录下的更新包已存在，拒绝覆盖：{}",
            destination.display()
        ));
    }
    let expected = release
        .package
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "发布包缺少有效的 SHA-256，已拒绝下载".to_string())?
        .to_ascii_lowercase();
    let file_name = destination
        .file_name()
        .ok_or_else(|| "下载目标必须是文件路径".to_string())?;
    let part_path = destination.with_file_name(format!("{}.part", file_name.to_string_lossy()));
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part_path)
        .map_err(|error| format!("无法创建临时下载文件：{error}"))?;

    let result = (|| {
        let response = get_request(&release.package.download_url, Duration::from_secs(120))
            .set("Accept", "application/octet-stream")
            .set("User-Agent", USER_AGENT)
            .call()
            .map_err(|error| format!("无法下载发布包：{error}"))?;
        if let Some(content_length) = response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok())
        {
            if content_length != release.package.size {
                return Err(format!(
                    "发布包长度与 GitHub 元数据不一致（预期 {}，实际 {content_length}）",
                    release.package.size
                ));
            }
        }

        let mut source = response.into_reader();
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut bytes = 0_u64;
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|error| format!("下载发布包时连接中断：{error}"))?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count as u64)
                .ok_or_else(|| "发布包长度溢出".to_string())?;
            if bytes > release.package.size || bytes > MAX_RELEASE_BYTES {
                return Err("下载内容超过 GitHub 声明的发布包大小".to_string());
            }
            target
                .write_all(&buffer[..count])
                .map_err(|error| format!("写入临时下载文件失败：{error}"))?;
            hasher.update(&buffer[..count]);
        }
        target
            .sync_all()
            .map_err(|error| format!("刷新临时下载文件失败：{error}"))?;
        if bytes != release.package.size {
            return Err(format!(
                "发布包下载不完整（预期 {} 字节，实际 {bytes} 字节）",
                release.package.size
            ));
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected {
            return Err(format!(
                "发布包 SHA-256 校验失败（预期 {expected}，实际 {actual}）"
            ));
        }
        fs::rename(&part_path, destination)
            .map_err(|error| format!("无法保存已校验的发布包：{error}"))?;
        Ok(DownloadReceipt {
            path: destination.to_path_buf(),
            transaction_dir: destination
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
            bytes,
            sha256: actual,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part_path);
    }
    result
}

fn check_for_update_at(api_url: &str, current_tag: &str) -> Result<UpdateCheck, String> {
    let response = get_request(api_url, Duration::from_secs(30))
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("无法连接 GitHub：{error}"))?;
    let release: GithubRelease = response
        .into_json()
        .map_err(|error| format!("GitHub 返回了无效的版本信息：{error}"))?;
    let package = release
        .assets
        .into_iter()
        .find(|asset| {
            asset.name.starts_with("hd2-armor-desk-windows-x64-") && asset.name.ends_with(".zip")
        })
        .ok_or_else(|| "最新 Release 中没有 Windows x64 ZIP 发布包".to_string())?;
    let release = ReleaseInfo {
        tag: release.tag_name,
        name: release.name,
        page_url: release.html_url,
        package: ReleasePackage {
            name: package.name,
            download_url: package.browser_download_url,
            size: package.size,
            digest: package.digest,
        },
    };
    let latest_version = hd2_updater::release_version(&release.tag)
        .map_err(|_| "最新 Release 的版本标签格式无效".to_string())?;
    if release.tag == current_tag {
        Ok(UpdateCheck::UpToDate(release))
    } else if hd2_updater::release_version(current_tag)
        .is_ok_and(|current_version| latest_version <= current_version)
    {
        Err("GitHub 最新 Release 不高于当前版本，已拒绝降级".to_string())
    } else {
        Ok(UpdateCheck::Available(release))
    }
}
#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::{
        check_for_update_at, download_release_to_inner, ReleaseInfo, ReleasePackage, UpdateCheck,
        RELEASES_PAGE_URL,
    };

    fn serve_json(body: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("local server address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept update request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read update request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write update response");
        });
        (format!("http://{address}/releases/latest"), handle)
    }

    fn serve_bytes(body: &'static [u8]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("local server address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept download request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read download request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write download headers");
            stream.write_all(body).expect("write package");
        });
        (format!("http://{address}/package.zip"), handle)
    }
    fn serve_truncated(
        declared_size: usize,
        body: &'static [u8],
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("local server address");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept download request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read download request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {declared_size}\r\nConnection: close\r\n\r\n"
            )
            .expect("write download headers");
            stream.write_all(body).expect("write partial package");
        });
        (format!("http://{address}/package.zip"), handle)
    }

    #[test]
    fn reports_the_latest_windows_package_as_an_available_update() {
        const RESPONSE: &str = r#"{
            "tag_name":"release-20260921-120000000-UTC8",
            "name":"HD2 Armor Desk 20260921-120000000-UTC8",
            "html_url":"https://github.com/lona-cn/hd2-sav-editor/releases/tag/release-20260921-120000000-UTC8",
            "assets":[
                {
                    "name":"hd2-armor-desk-windows-x64-20260921-120000000-UTC8.zip.sha256",
                    "browser_download_url":"https://github.com/lona-cn/hd2-sav-editor/releases/download/release-20260921-120000000-UTC8/package.zip.sha256",
                    "size":120,
                    "digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "name":"hd2-armor-desk-windows-x64-20260921-120000000-UTC8.zip",
                    "browser_download_url":"https://github.com/lona-cn/hd2-sav-editor/releases/download/release-20260921-120000000-UTC8/package.zip",
                    "size":7000000,
                    "digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            ]
        }"#;
        let (url, server) = serve_json(RESPONSE);

        let result = check_for_update_at(&url, "release-20260920-120000000-UTC8")
            .expect("valid GitHub response should be accepted");
        server.join().expect("test server should finish");

        let UpdateCheck::Available(release) = result else {
            panic!("a different latest release should be available");
        };
        assert_eq!(release.tag, "release-20260921-120000000-UTC8");
        assert_eq!(
            release.package.name,
            "hd2-armor-desk-windows-x64-20260921-120000000-UTC8.zip"
        );
        assert_eq!(release.package.size, 7_000_000);
    }

    #[test]
    fn refuses_to_offer_an_older_release_as_an_update() {
        const RESPONSE: &str = r#"{
            "tag_name":"release-20260919-120000000-UTC8",
            "name":"HD2 Armor Desk 20260919-120000000-UTC8",
            "html_url":"https://github.com/lona-cn/hd2-sav-editor/releases/tag/release-20260919-120000000-UTC8",
            "assets":[{
                "name":"hd2-armor-desk-windows-x64-20260919-120000000-UTC8.zip",
                "browser_download_url":"https://github.com/lona-cn/hd2-sav-editor/releases/download/release-20260919-120000000-UTC8/package.zip",
                "size":10,
                "digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }"#;
        let (url, server) = serve_json(RESPONSE);

        let error = check_for_update_at(&url, "release-20260920-120000000-UTC8")
            .expect_err("an older latest release must not be installed");
        server.join().expect("test server should finish");
        assert!(error.contains("拒绝降级"));
    }

    #[test]
    fn downloads_the_package_only_after_its_sha256_matches() {
        let (download_url, server) = serve_bytes(b"hello");
        let release = ReleaseInfo {
            tag: "release-20260921-120000000-UTC8".to_string(),
            name: "HD2 Armor Desk 20260921-120000000-UTC8".to_string(),
            page_url: RELEASES_PAGE_URL.to_string(),
            package: ReleasePackage {
                name: "package.zip".to_string(),
                download_url,
                size: 5,
                digest: Some(
                    "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                        .to_string(),
                ),
            },
        };
        let destination = std::env::temp_dir().join(format!(
            "hd2-armor-desk-update-test-{}.zip",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&destination);

        let receipt = download_release_to_inner(&release, &destination)
            .expect("verified download should succeed");
        server.join().expect("test server should finish");

        assert_eq!(std::fs::read(&destination).unwrap(), b"hello");
        assert_eq!(
            receipt.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(receipt.bytes, 5);
        std::fs::remove_file(destination).unwrap();
    }

    #[test]
    fn removes_partial_downloads_after_a_connection_ends_early() {
        let (download_url, server) = serve_truncated(12, b"short");
        let release = ReleaseInfo {
            tag: "release-20260921-120000000-UTC8".to_string(),
            name: "HD2 Armor Desk 20260921-120000000-UTC8".to_string(),
            page_url: RELEASES_PAGE_URL.to_string(),
            package: ReleasePackage {
                name: "package.zip".to_string(),
                download_url,
                size: 12,
                digest: Some(
                    "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                        .to_string(),
                ),
            },
        };
        let destination = std::env::temp_dir().join(format!(
            "hd2-armor-desk-truncated-update-test-{}.zip",
            std::process::id()
        ));
        let part_path = destination.with_file_name("hd2-armor-desk-truncated-update-test.part");
        let _ = std::fs::remove_file(&destination);
        let _ = std::fs::remove_file(&part_path);

        let result = download_release_to_inner(&release, &destination);
        server.join().expect("test server should finish");

        assert!(result.is_err());
        assert!(!destination.exists());
        assert!(!part_path.exists());
    }
}
